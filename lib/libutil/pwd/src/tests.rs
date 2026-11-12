use super::CHAOS_HOME_DIR_NAME;
use super::CHAOS_HOME_ENV_VAR;
use super::find_chaos_home_from_env;
use dirs::home_dir;
use pretty_assertions::assert_eq;
use std::io::ErrorKind;
use tempfile::TempDir;

#[test]
fn find_chaos_home_env_rejects_invalid_paths() {
    let temp_home = TempDir::new().expect("temp home");
    let missing = temp_home.path().join("missing-chaos-home");
    let file_path = temp_home.path().join("chaos-home.txt");
    std::fs::write(&file_path, "not a directory").expect("write temp file");

    for (label, path, kind, message) in [
        (
            "missing path",
            missing,
            ErrorKind::NotFound,
            CHAOS_HOME_ENV_VAR,
        ),
        (
            "file path",
            file_path,
            ErrorKind::InvalidInput,
            "not a directory",
        ),
    ] {
        let raw = path
            .to_str()
            .expect("CHAOS_HOME path should be valid utf-8");
        let err = find_chaos_home_from_env(Some(raw)).expect_err(label);
        assert_eq!(err.kind(), kind, "{label}");
        assert!(
            err.to_string().contains(message),
            "{label}: unexpected error: {err}"
        );
    }
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
fn find_chaos_auth_home_defaults_to_chaos_home() {
    let chaos_home = TempDir::new().expect("temp chaos home");
    let resolved =
        super::find_chaos_auth_home_from_env(chaos_home.path(), None).expect("default auth home");
    assert_eq!(resolved, chaos_home.path().to_path_buf());
}

#[test]
fn find_chaos_auth_home_uses_env_directory() {
    let chaos_home = TempDir::new().expect("temp chaos home");
    let auth_home = TempDir::new().expect("temp auth home");
    let raw = auth_home.path().to_str().expect("utf-8 path");
    let resolved = super::find_chaos_auth_home_from_env(chaos_home.path(), Some(raw))
        .expect("auth home from env");
    assert_eq!(
        resolved,
        auth_home
            .path()
            .canonicalize()
            .expect("canonical auth home")
    );
}

#[test]
fn find_chaos_auth_home_rejects_missing_directory_without_fallback() {
    let chaos_home = TempDir::new().expect("temp chaos home");
    let missing = chaos_home.path().join("missing-auth-home");
    let raw = missing.to_str().expect("utf-8 path");
    let err = super::find_chaos_auth_home_from_env(chaos_home.path(), Some(raw))
        .expect_err("missing auth home must be an error");
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(
        err.to_string().contains(super::CHAOS_AUTH_HOME_ENV_VAR),
        "unexpected error: {err}"
    );
}
