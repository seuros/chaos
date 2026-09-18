use super::*;
use chaos_keyring::tests::MockKeyringStore;
use pretty_assertions::assert_eq;

#[test]
fn lib_suite() {
    environment_id_fallback_has_cwd_prefix();
    manager_round_trips_local_backend().expect("manager_round_trips_local_backend");
}

fn environment_id_fallback_has_cwd_prefix() {
    #[cfg(target_os = "macos")]
    let temp_root = Path::new("/private/tmp");
    #[cfg(not(target_os = "macos"))]
    let temp_root = Path::new("/tmp");
    let dir = tempfile::tempdir_in(temp_root).expect("tempdir");
    let env_id = environment_id_from_cwd(dir.path());
    let canonical = dir
        .path()
        .canonicalize()
        .expect("tempdir canonical path should exist")
        .to_string_lossy()
        .into_owned();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let hex = digest_hex(&digest);
    let short = hex.get(..12).expect("digest has at least 12 chars");
    assert_eq!(env_id, format!("cwd-{short}"));
}

fn manager_round_trips_local_backend() -> Result<()> {
    let chaos_home = tempfile::tempdir().expect("tempdir");
    let keyring = Arc::new(MockKeyringStore::default());
    let manager = SecretsManager::new_with_keyring_store(
        chaos_home.path().to_path_buf(),
        SecretsBackendKind::Local,
        keyring,
    );
    let scope = SecretScope::Global;
    let name = SecretName::new("GITHUB_TOKEN")?;

    manager.set(&scope, &name, "token-1")?;
    assert_eq!(manager.get(&scope, &name)?, Some("token-1".to_string()));

    let listed = manager.list(None)?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, name);

    assert!(manager.delete(&scope, &name)?);
    assert_eq!(manager.get(&scope, &name)?, None);
    Ok(())
}
