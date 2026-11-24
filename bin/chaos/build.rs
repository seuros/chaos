use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn watch_git_path(root: &Path, name: &str) {
    // Missing loose refs can be packed. Watch their nearest existing parent.
    if let Some(path) = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", name],
    ) && let Some(existing) = Path::new(path.trim()).ancestors().find(|p| p.exists())
    {
        println!("cargo::rerun-if-changed={}", existing.display());
    }
}

fn source_version(root: &Path) -> Option<String> {
    // An exported source tree may itself be inside an unrelated Git checkout.
    if !root.join(".git").exists() {
        return None;
    }
    for name in ["HEAD", "index", "packed-refs"] {
        watch_git_path(root, name);
    }
    if let Some(reference) = git(root, &["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(root, reference.trim());
    }
    // Git metadata alone does not change when an unstaged file is edited.
    if let Some(files) = git(root, &["ls-files", "-z"]) {
        for path in files
            .split_terminator('\0')
            .filter(|p| !p.contains(['\n', '\r']))
        {
            println!("cargo::rerun-if-changed={}", root.join(path).display());
        }
    }
    let sha = git(root, &["rev-parse", "--short=8", "HEAD"])?;
    let sha = sha.trim();
    if sha.len() < 8 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let status = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "-uno",
            "--ignore-submodules=untracked",
        ],
    )?;
    let suffix = if status.is_empty() { "" } else { "-dirty" };
    Some(format!("{sha}{suffix}"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let root = manifest.join("../..").canonicalize()?;
    let revision = source_version(&root).unwrap_or_else(|| "unknown".to_owned());
    let version = env::var("CARGO_PKG_VERSION")?;
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=CHAOS_RELEASE_TAG");
    println!("cargo::rustc-env=CHAOS_BUILD_VERSION={version}+{revision}");
    Ok(())
}
