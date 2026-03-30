use dirs::home_dir;
use std::path::PathBuf;

pub const CHAOS_HOME_ENV_VAR: &str = "CHAOS_HOME";
pub const CHAOS_HOME_DIR_NAME: &str = ".chaos";

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
mod tests {
    use super::CHAOS_HOME_DIR_NAME;
    use super::CHAOS_HOME_ENV_VAR;
    use super::find_chaos_home_from_env;
    use dirs::home_dir;
    use pretty_assertions::assert_eq;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn find_chaos_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-codex-home");
        let missing_str = missing
            .to_str()
            .expect("missing chaos home path should be valid utf-8");

        let err = find_chaos_home_from_env(Some(missing_str)).expect_err("missing CHAOS_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains(CHAOS_HOME_ENV_VAR),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_chaos_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("chaos-home.txt");
        std::fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file chaos home path should be valid utf-8");

        let err = find_chaos_home_from_env(Some(file_str)).expect_err("file CHAOS_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_chaos_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp chaos home path should be valid utf-8");

        let resolved = find_chaos_home_from_env(Some(temp_str)).expect("valid CHAOS_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_chaos_home_without_env_uses_default_chaos_home_dir() {
        let resolved = find_chaos_home_from_env(None).expect("default home selection");
        let expected = home_dir().expect("home dir").join(CHAOS_HOME_DIR_NAME);
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_chaos_home_uses_chaos_env() {
        let temp_home = TempDir::new().expect("temp home");
        let chaos_home = temp_home.path().join(CHAOS_HOME_DIR_NAME);
        std::fs::create_dir_all(&chaos_home).expect("create chaos home");
        let resolved =
            find_chaos_home_from_env(Some(chaos_home.to_str().expect("utf8 chaos path")))
                .expect("chaos env home");
        assert_eq!(
            resolved,
            chaos_home.canonicalize().expect("canonicalize chaos home")
        );
    }
}
