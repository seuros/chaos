use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum CargoBinError {
    #[error("failed to read current exe")]
    CurrentExe {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read current directory")]
    CurrentDir {
        #[source]
        source: std::io::Error,
    },
    #[error("CARGO_BIN_EXE env var {key} resolved to {path:?}, but it does not exist")]
    ResolvedPathDoesNotExist { key: String, path: PathBuf },
    #[error("could not locate binary {name:?}; tried env vars {env_keys:?}; {fallback}")]
    NotFound {
        name: String,
        env_keys: Vec<String>,
        fallback: String,
    },
}

/// Returns an absolute path to a binary target built for the current test run.
///
/// In `cargo test`, `CARGO_BIN_EXE_*` env vars are absolute paths.
#[allow(deprecated)]
pub fn cargo_bin(name: &str) -> Result<PathBuf, CargoBinError> {
    let env_keys = cargo_bin_env_keys(name);
    for key in &env_keys {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(&value);
            if path.is_absolute() && path.exists() {
                return Ok(path);
            }
            return Err(CargoBinError::ResolvedPathDoesNotExist {
                key: key.to_owned(),
                path,
            });
        }
    }
    if let Some(path) = legacy_cargo_bin_path(name)? {
        return Ok(path);
    }

    if build_cargo_bin(name).is_ok()
        && let Some(path) = legacy_cargo_bin_path(name)?
    {
        return Ok(path);
    }

    Err(CargoBinError::NotFound {
        name: name.to_owned(),
        env_keys,
        fallback: "env vars were unset and target/debug fallback did not exist".to_string(),
    })
}

fn cargo_bin_env_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    keys.push(format!("CARGO_BIN_EXE_{name}"));

    // Cargo replaces dashes in target names when exporting env vars.
    let underscore_name = name.replace('-', "_");
    if underscore_name != name {
        keys.push(format!("CARGO_BIN_EXE_{underscore_name}"));
    }

    keys
}

fn legacy_cargo_bin_path(name: &str) -> Result<Option<PathBuf>, CargoBinError> {
    let bin_name = format!("{}{}", name, std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();

    let mut current_exe =
        std::env::current_exe().map_err(|source| CargoBinError::CurrentExe { source })?;
    current_exe.pop();
    if current_exe.ends_with("deps") {
        current_exe.pop();
    }
    candidates.push(current_exe.join(&bin_name));

    if let Ok(root) = repo_root() {
        candidates.push(root.join("target").join("debug").join(&bin_name));
    }

    Ok(candidates.into_iter().find(|path| path.exists()))
}

fn build_cargo_bin(name: &str) -> io::Result<()> {
    let root = repo_root()?;
    let status = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("--bin")
        .arg(name)
        .current_dir(root)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "cargo build --bin {name} exited with {status}"
        )))
    }
}

/// Macro that derives the path to a test resource at compile time using
/// `CARGO_MANIFEST_DIR`. This is expected to be used exclusively in test code.
#[macro_export]
macro_rules! find_resource {
    ($resource:expr) => {{
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        Ok::<std::path::PathBuf, std::io::Error>(manifest_dir.join($resource))
    }};
}

pub fn repo_root() -> io::Result<PathBuf> {
    let marker = Path::new(env!("CARGO_MANIFEST_DIR")).join("repo_root.marker");
    let mut root = marker;
    for _ in 0..4 {
        root = root
            .parent()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "repo_root.marker did not have expected parent depth",
                )
            })?
            .to_path_buf();
    }
    Ok(root)
}
