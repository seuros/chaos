//! The two-key bootstrap file: where the runtime database lives and which
//! egress gateway to use. Everything else is a database setting.
use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const BOOTSTRAP_KEYS: &[&str] = &["storage_url", "egress_url"];

const STORAGE_URL_KEY: &str = "storage_url";
const EGRESS_URL_KEY: &str = "egress_url";
const SQLITE_HOME_KEY: &str = "sqlite_home";
const STORAGE_URL_ENV: &str = "CHAOS_STORAGE_URL";
const BOOTSTRAP_LOCK_FILE: &str = "bootstrap.lock";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_url: Option<String>,
}

/// Bootstrap values with references resolved, ready for storage and providers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBootstrap {
    /// Normalized storage URL. Explicit: there is no silent SQLite default.
    pub storage_url: String,
    /// Directory keying the mounted backend: `CHAOS_SQLITE_HOME` or the home.
    /// SQLite stores `chaos.sqlite` there; PostgreSQL only uses it as a key.
    pub storage_home: PathBuf,
    /// Egress gateway from the bootstrap file; `CHAOS_EGRESS_URL` overrides later.
    pub egress_url: Option<String>,
}

impl ResolvedBootstrap {
    /// SQLite inside `home`, for harnesses and tests that own a temporary home.
    pub fn sqlite_in(home: &Path) -> Self {
        Self {
            storage_url: sqlite_url_in(home),
            storage_home: home.to_path_buf(),
            egress_url: None,
        }
    }
}

fn sqlite_url_in(dir: &Path) -> String {
    format!("sqlite://{}", dir.join("chaos.sqlite").display())
}

impl BootstrapConfig {
    pub fn path(home: &Path) -> PathBuf {
        home.join(chaos_sysctl::CONFIG_TOML_FILE)
    }

    /// Strict read: only `storage_url` and `egress_url` are accepted.
    pub fn read(home: &Path) -> anyhow::Result<Self> {
        let path = Self::path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err.into()),
        };
        let table: toml::Table = toml::from_str(&text)
            .with_context(|| format!("{}: invalid bootstrap file", path.display()))?;
        for key in table.keys() {
            if BOOTSTRAP_KEYS.contains(&key.as_str()) {
                continue;
            }
            if key == SQLITE_HOME_KEY {
                bail!(
                    "{}: `{SQLITE_HOME_KEY}` was removed. Remove it from the file, then set \
                     the CHAOS_SQLITE_HOME environment variable or run \
                     `chaos config bootstrap set storage_url sqlite:///<dir>/chaos.sqlite`.",
                    path.display()
                );
            }
            bail!(
                "{}: `{key}` is not a bootstrap setting. This file holds only `storage_url` \
                 and `egress_url`; user settings live in the runtime database. Remove `{key}` \
                 from the file and set it with `chaos config set {key} <json>`. Bootstrap \
                 values are edited with `chaos config bootstrap set`.",
                path.display()
            );
        }
        if let Some(error) = chaos_sysctl::config_error_from_typed_toml::<Self>(&path, &text) {
            bail!("{}", chaos_sysctl::format_config_error(&error, &text));
        }
        Ok(table.try_into()?)
    }

    pub fn redacted(mut self) -> Self {
        for value in [&mut self.storage_url, &mut self.egress_url]
            .into_iter()
            .flatten()
        {
            if value.starts_with("env:") || chaos_sysctl::secrets::is_reference(value) {
                continue;
            }
            if let Ok(mut url) = url::Url::parse(value) {
                if !url.username().is_empty() || url.password().is_some() {
                    let _ = url.set_username("REDACTED");
                    let _ = url.set_password(None);
                }
                if url.query().is_some() {
                    url.set_query(Some("REDACTED"));
                }
                *value = url.into();
            } else {
                *value = "<invalid URL; value redacted>".into();
            }
        }
        self
    }

    pub fn has_storage_url(&self) -> bool {
        self.storage_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
    }

    /// `CHAOS_SQLITE_HOME` (relative paths join the home), else the home itself.
    pub fn storage_home(&self, home: &Path) -> PathBuf {
        sqlite_home_for(home)
    }

    /// Bootstrap `storage_url`, then `CHAOS_STORAGE_URL`, then a SQLite file
    /// under `CHAOS_SQLITE_HOME`. Returns `None` when nothing is configured.
    pub fn resolved_storage_url(&self, home: &Path) -> anyhow::Result<Option<String>> {
        if let Some(value) = self.storage_url.as_deref().filter(|v| !v.trim().is_empty()) {
            return resolve_reference(home, value.trim()).map(Some);
        }
        if let Some(value) = std::env::var(STORAGE_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            return Ok(Some(value.trim().to_owned()));
        }
        if chaos_proc::sqlite_home_env_value().is_some() {
            return Ok(Some(sqlite_url_in(&sqlite_home_for(home))));
        }
        Ok(None)
    }

    pub fn resolve(&self, home: &Path) -> anyhow::Result<ResolvedBootstrap> {
        let Some(storage_url) = self.resolved_storage_url(home)? else {
            bail!(
                "No runtime storage is configured for {}. Run `chaos` once to choose \
                 PostgreSQL or SQLite, or run `chaos config bootstrap set storage_url \
                 <postgres://...|sqlite:///path/chaos.sqlite|env:VAR>`, or set \
                 {STORAGE_URL_ENV}. See docs/getting-started.md.",
                home.display()
            );
        };
        let storage_url = crate::config::normalize_storage_url(Some(&storage_url))?
            .context("storage_url is empty")?;
        let egress_url = self
            .egress_url
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|value| resolve_reference(home, value.trim()))
            .transpose()?;
        Ok(ResolvedBootstrap {
            storage_url,
            storage_home: sqlite_home_for(home),
            egress_url,
        })
    }
}

fn sqlite_home_for(home: &Path) -> PathBuf {
    match chaos_proc::sqlite_home_env_value().map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => home.join(path),
        None => home.to_path_buf(),
    }
}

/// Read and resolve bootstrap off the async runtime: credential references can
/// block on the OS unlock UI.
pub async fn resolve_bootstrap(home: &Path) -> anyhow::Result<ResolvedBootstrap> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || BootstrapConfig::read(&home)?.resolve(&home))
        .await
        .context("bootstrap resolution task failed")?
}

pub(crate) fn resolve_reference(home: &Path, value: &str) -> anyhow::Result<String> {
    if let Some(name) = value.strip_prefix("env:") {
        ensure!(!name.is_empty(), "empty environment reference");
        return std::env::var(name)
            .with_context(|| format!("missing bootstrap environment variable {name}"));
    }
    chaos_sysctl::secrets::resolve(home, value)
}

pub(crate) fn lock_bootstrap(home: &Path) -> anyhow::Result<std::fs::File> {
    std::fs::create_dir_all(home)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(home.join(BOOTSTRAP_LOCK_FILE))?;
    lock.try_lock()
        .context("another bootstrap edit is running")?;
    Ok(lock)
}

/// Validate and persist one bootstrap key. Credential-bearing URLs must use
/// `env:VARIABLE` or a credential reference.
pub fn set_bootstrap(home: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    ensure!(
        matches!(key, STORAGE_URL_KEY | EGRESS_URL_KEY),
        "only storage_url and egress_url are bootstrap settings"
    );
    if let Some(name) = value.strip_prefix("env:") {
        ensure!(!name.is_empty(), "empty environment reference");
    } else if chaos_sysctl::secrets::is_reference(value) {
        uuid::Uuid::parse_str(value.rsplit('/').next().unwrap_or_default())
            .context("invalid credential reference")?;
    } else {
        let url = url::Url::parse(value)
            .context("bootstrap value must be a URL or credential/environment reference")?;
        ensure!(
            url.password().is_none(),
            "use env:VARIABLE for credential-bearing URLs"
        );
        if key == STORAGE_URL_KEY {
            crate::config::normalize_storage_url(Some(value))?;
        } else {
            chaos_client::Egress::parse(value).map_err(anyhow::Error::msg)?;
        }
    }
    let _lock = lock_bootstrap(home)?;
    let value = value.to_owned();
    write_bootstrap(home, |bootstrap| {
        if key == STORAGE_URL_KEY {
            bootstrap.storage_url = Some(value);
        } else {
            bootstrap.egress_url = Some(value);
        }
    })
}

/// Rewrite the bootstrap file through its symlink target, atomically. The
/// strict read runs first so a legacy file is never silently truncated.
pub(crate) fn write_bootstrap(
    home: &Path,
    update: impl FnOnce(&mut BootstrapConfig),
) -> anyhow::Result<()> {
    let mut bootstrap = BootstrapConfig::read(home)?;
    update(&mut bootstrap);
    std::fs::create_dir_all(home)?;
    let path = BootstrapConfig::path(home);
    let write_path = chaos_sysctl::path_utils::resolve_symlink_write_paths(&path)?.write_path;
    chaos_sysctl::path_utils::write_atomically(&write_path, &toml::to_string_pretty(&bootstrap)?)
        .with_context(|| format!("failed to persist {}", write_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
