//! `chaos update`: replace the installed release bundle with a newer GitHub
//! release. Runs only when invoked; nothing checks for updates in the
//! background.

use std::cmp::Ordering;
use std::fs;
use std::io::IsTerminal as _;
use std::io::Read as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use anyhow::bail;
use anyhow::ensure;
use chaos_client::ChaosHttpClient;
use serde::Deserialize;
use sha2::Digest as _;
use sha2::Sha256;

const DEFAULT_REPO: &str = "seuros/chaos";
const BUNDLE: [&str; 4] = [
    "chaos",
    "alcatraz",
    "chaos_journald",
    "chaos-forkve-wrapper",
];
const MAX_REDIRECTS: usize = 5;
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;
const CURRENT_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), ".", env!("CHAOS_BUILD_TS"));

#[derive(Debug, usage::Args)]
pub struct UpdateCommand {
    /// Only report whether a newer release exists.
    #[usage(long)]
    pub check: bool,

    /// Install this release tag instead of the latest (e.g. v47.13.0.1791378219).
    #[usage(long = "tag", value_name = "TAG")]
    pub tag: Option<String>,

    /// Reinstall even when the release is not newer than the running binary.
    #[usage(long)]
    pub force: bool,

    /// Install without asking for confirmation.
    #[usage(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
}

pub async fn run(cmd: UpdateCommand) -> anyhow::Result<()> {
    let repo = std::env::var("CHAOS_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_owned());
    let client = ChaosHttpClient::default_client();

    let tag = match cmd.tag {
        Some(tag) => tag,
        None => latest_tag(&client, &repo).await?,
    };
    let current = ReleaseVersion::parse(CURRENT_VERSION)
        .with_context(|| format!("unparseable build version {CURRENT_VERSION}"))?;
    let candidate =
        ReleaseVersion::parse(&tag).with_context(|| format!("unparseable release tag {tag}"))?;
    let newer = candidate.cmp(&current) == Ordering::Greater;

    if cmd.check {
        if newer {
            println!("update available: v{CURRENT_VERSION} -> {tag}");
        } else {
            println!("up to date: v{CURRENT_VERSION} (latest {tag})");
        }
        return Ok(());
    }
    if !newer && !cmd.force {
        println!("up to date: v{CURRENT_VERSION} (latest {tag}); use --force to reinstall");
        return Ok(());
    }

    let install_dir = install_dir()?;
    let target = release_target()?;
    let verb = if newer { "update" } else { "replace" };
    if !cmd.yes {
        ensure!(
            std::io::stdin().is_terminal(),
            "stdin is not a terminal; pass --yes to update non-interactively"
        );
        let question = format!(
            "{verb} v{CURRENT_VERSION} -> {tag} in {}? [y/N]: ",
            install_dir.display()
        );
        if !crate::confirm(&question)? {
            println!("aborted");
            return Ok(());
        }
    }
    let archive = format!("chaos-{tag}-{target}.tar.gz");
    let url = format!("https://github.com/{repo}/releases/download/{tag}/{archive}");

    println!("downloading {url}");
    let bytes = fetch(&client, &url).await?;
    let manifest = fetch(&client, &format!("{url}.sha256")).await?;
    verify_checksum(&bytes, &manifest)?;
    println!("verified SHA-256");

    let staging = tempfile::Builder::new()
        .prefix(".chaos-update-")
        .tempdir_in(&install_dir)
        .with_context(|| format!("{} is not writable", install_dir.display()))?;
    unpack_bundle(&bytes, staging.path())?;
    for name in BUNDLE {
        let staged = staging.path().join(name);
        // rename(2) within one directory is atomic, so a running chaos keeps
        // its old inode and the next exec sees the complete new binary.
        fs::rename(&staged, install_dir.join(name))
            .with_context(|| format!("failed to install {name}"))?;
    }
    println!(
        "updated v{CURRENT_VERSION} -> {tag} in {}",
        install_dir.display()
    );
    Ok(())
}

async fn latest_tag(client: &ChaosHttpClient, repo: &str) -> anyhow::Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "chaos")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    if status.as_u16() == 404 {
        bail!("no published release for {repo}");
    }
    ensure!(status.is_success(), "GET {url}: HTTP {status}");
    let release: Release = response.json().await.context("invalid release metadata")?;
    Ok(release.tag_name)
}

/// GET following GitHub's asset redirects by hand; the shared client never
/// follows them itself, and an update must never be downgraded to plain HTTP.
async fn fetch(client: &ChaosHttpClient, url: &str) -> anyhow::Result<Vec<u8>> {
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        ensure!(url.starts_with("https://"), "refusing non-HTTPS URL {url}");
        let response = client
            .get(&url)
            .header("User-Agent", "chaos")
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if status.is_redirection() {
            url = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .with_context(|| format!("GET {url}: redirect without Location"))?
                .to_owned();
            continue;
        }
        ensure!(status.is_success(), "GET {url}: HTTP {status}");
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("GET {url}"))?;
        ensure!(
            bytes.len() <= MAX_ARCHIVE_BYTES,
            "GET {url}: response too large"
        );
        return Ok(bytes.to_vec());
    }
    bail!("too many redirects fetching {url}")
}

/// Parses the `sha256sum` line `<digest>  <file>` and checks the archive.
pub(crate) fn verify_checksum(archive: &[u8], manifest: &[u8]) -> anyhow::Result<()> {
    let manifest = std::str::from_utf8(manifest).context("invalid SHA-256 manifest")?;
    let mut lines = manifest.lines();
    let expected = lines
        .next()
        .and_then(|line| line.split_whitespace().next())
        .context("invalid SHA-256 manifest")?;
    ensure!(lines.next().is_none(), "invalid SHA-256 manifest");
    ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid SHA-256 digest"
    );
    let actual: String = Sha256::digest(archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        "release archive SHA-256 mismatch; nothing installed"
    );
    Ok(())
}

/// Extracts only the bundle binaries as regular files; anything else in the
/// archive is ignored and a missing binary aborts before installation.
pub(crate) fn unpack_bundle(archive: &[u8], dest: &Path) -> anyhow::Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut found = [false; BUNDLE.len()];
    for entry in tar.entries().context("corrupt release archive")? {
        let mut entry = entry.context("corrupt release archive")?;
        let path = entry
            .path()
            .context("corrupt release archive")?
            .into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(idx) = BUNDLE.iter().position(|b| *b == name) else {
            continue;
        };
        if path.components().count() > 2 || !entry.header().entry_type().is_file() {
            continue;
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .with_context(|| format!("failed to read {name}"))?;
        write_executable(&dest.join(name), &data)?;
        found[idx] = true;
    }
    if let Some(idx) = found.iter().position(|f| !f) {
        bail!("release archive is missing a regular file: {}", BUNDLE[idx]);
    }
    Ok(())
}

fn write_executable(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::write(path, data).with_context(|| format!("failed to write {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("failed to chmod {}", path.display()))
}

/// Directory holding the running bundle, refusing installs another tool owns.
fn install_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    if let Some(reason) = managed_install(&exe) {
        bail!("{} {reason}", exe.display());
    }
    let dir = exe.parent().context("binary has no parent directory")?;
    Ok(dir.to_path_buf())
}

pub(crate) fn managed_install(exe: &Path) -> Option<&'static str> {
    let path = exe.to_string_lossy();
    if path.starts_with("/nix/store/") {
        return Some("is managed by Nix; update the flake input instead");
    }
    if path.contains("/.cargo/bin/") {
        return Some("was installed by cargo; rerun cargo install instead");
    }
    let in_target = exe.ancestors().any(|dir| {
        dir.file_name().is_some_and(|n| n == "target")
            && dir.parent().is_some_and(|p| p.join("Cargo.toml").exists())
    });
    if in_target {
        return Some("is a source build; git pull and rebuild instead");
    }
    None
}

/// Target triple matching the release asset names in install.sh.
fn release_target() -> anyhow::Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("freebsd", "x86_64") => "x86_64-unknown-freebsd",
        (os, arch) => bail!("no prebuilt release for {os}/{arch}"),
    })
}

/// `v<major>.<minor>.<patch>.<build_ts>`, as produced by the release workflow.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ReleaseVersion([u64; 4]);

impl ReleaseVersion {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.strip_prefix('v').unwrap_or(raw).split('.');
        let mut out = [0u64; 4];
        for slot in &mut out {
            *slot = parts.next()?.parse().ok()?;
        }
        parts.next().is_none().then_some(Self(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_checksum_and_bundle() {
        let v = |s| ReleaseVersion::parse(s);
        assert!(v("v47.13.0.1791378219") > v("47.13.0.1791378218"));
        assert!(v("v47.14.0.1") > v("v47.13.9.9999999999"));
        assert_eq!(v("build-7d1ccbd577"), None);
        assert_eq!(v("v47.13.0"), None);

        let mut gz = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            let mut tar = tar::Builder::new(enc);
            for name in BUNDLE.iter().chain(["install.sh"].iter()) {
                let mut header = tar::Header::new_gnu();
                header.set_size(4);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, format!("./{name}"), &b"elf!"[..])
                    .unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        let digest: String = Sha256::digest(&gz)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        verify_checksum(&gz, format!("{digest}  chaos.tar.gz\n").as_bytes()).unwrap();
        assert!(verify_checksum(b"tampered", format!("{digest}  x\n").as_bytes()).is_err());
        assert!(verify_checksum(&gz, format!("{digest}  x\n{digest}  y\n").as_bytes()).is_err());

        let dir = tempfile::tempdir().unwrap();
        unpack_bundle(&gz, dir.path()).unwrap();
        assert!(!dir.path().join("install.sh").exists());
        assert_eq!(fs::read(dir.path().join("alcatraz")).unwrap(), b"elf!");

        assert!(managed_install(Path::new("/nix/store/abc-chaos/bin/chaos")).is_some());
        assert!(managed_install(Path::new("/home/u/.cargo/bin/chaos")).is_some());
        assert!(managed_install(Path::new("/home/u/.local/bin/chaos")).is_none());
    }
}
