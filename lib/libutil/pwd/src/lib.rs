use dirs::home_dir;
use std::path::Path;
use std::path::PathBuf;

pub const CHAOS_HOME_ENV_VAR: &str = "CHAOS_HOME";
pub const CHAOS_HOME_DIR_NAME: &str = ".chaos";
/// Optional directory that holds provider credentials instead of `CHAOS_HOME`.
///
/// Lets one Chaos home (sessions, history, settings, trust, hooks) serve runs
/// that authenticate differently, e.g. an API key and a ChatGPT subscription
/// login for the same provider, by pointing each run at its own credentials
/// directory.
pub const CHAOS_AUTH_HOME_ENV_VAR: &str = "CHAOS_AUTH_HOME";

/// Returns the path to the ChaOS configuration directory.
///
/// Resolution order:
/// 1. `CHAOS_HOME`
/// 2. `~/.chaos`
///
/// - If `CHAOS_HOME` is set, the value must exist and be a directory. The value
///   will be canonicalized and this function will Err otherwise.
/// - If `CHAOS_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_chaos_home() -> std::io::Result<PathBuf> {
    let chaos_home_env = std::env::var(CHAOS_HOME_ENV_VAR)
        .ok()
        .filter(|val| !val.is_empty());
    find_chaos_home_from_env(chaos_home_env.as_deref())
}

fn find_chaos_home_from_env(chaos_home_env: Option<&str>) -> std::io::Result<PathBuf> {
    if let Some(val) = chaos_home_env {
        return validate_env_home(val, CHAOS_HOME_ENV_VAR);
    }

    let home = home_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not find home directory",
        )
    })?;
    Ok(home.join(CHAOS_HOME_DIR_NAME))
}

/// Returns the directory that holds provider credentials for `chaos_home`.
///
/// - If `CHAOS_AUTH_HOME` is set and non-empty, it must exist and be a
///   directory; it is canonicalized and returned. An invalid value is an
///   error, never a silent fallback to `chaos_home`: falling back would let a
///   run read or overwrite credentials that belong to another login.
/// - Otherwise `chaos_home` is returned unchanged.
pub fn find_chaos_auth_home(chaos_home: &Path) -> std::io::Result<PathBuf> {
    let auth_home_env = std::env::var(CHAOS_AUTH_HOME_ENV_VAR)
        .ok()
        .filter(|val| !val.is_empty());
    find_chaos_auth_home_from_env(chaos_home, auth_home_env.as_deref())
}

pub fn find_chaos_auth_home_from_env(
    chaos_home: &Path,
    auth_home_env: Option<&str>,
) -> std::io::Result<PathBuf> {
    match auth_home_env {
        Some(val) => validate_env_home(val, CHAOS_AUTH_HOME_ENV_VAR),
        None => Ok(chaos_home.to_path_buf()),
    }
}

fn validate_env_home(raw: &str, env_var: &str) -> std::io::Result<PathBuf> {
    let path = PathBuf::from(raw);
    let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{env_var} points to {raw:?}, but that path does not exist"),
        ),
        _ => std::io::Error::new(
            err.kind(),
            format!("failed to read {env_var} {raw:?}: {err}"),
        ),
    })?;

    if !metadata.is_dir() {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{env_var} points to {raw:?}, but that path is not a directory"),
        ))
    } else {
        path.canonicalize().map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("failed to canonicalize {env_var} {raw:?}: {err}"),
            )
        })
    }
}

#[cfg(test)]
mod tests;
