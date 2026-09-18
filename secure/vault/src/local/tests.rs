use super::*;
use chaos_keyring::tests::MockKeyringStore;
use keyring_core::Error as KeyringError;
use pretty_assertions::assert_eq;

#[test]
fn local_suite() {
    load_file_rejects_newer_schema_versions().expect("load_file_rejects_newer_schema_versions");
    set_fails_when_keyring_is_unavailable().expect("set_fails_when_keyring_is_unavailable");
    save_file_does_not_leave_temp_files().expect("save_file_does_not_leave_temp_files");
}

fn load_file_rejects_newer_schema_versions() -> Result<()> {
    let chaos_home = tempfile::tempdir().expect("tempdir");
    let keyring = Arc::new(MockKeyringStore::default());
    let backend = LocalSecretsBackend::new(chaos_home.path().to_path_buf(), keyring);

    let file = SecretsFile {
        version: SECRETS_VERSION + 1,
        secrets: BTreeMap::new(),
    };
    backend.save_file(&file)?;

    let error = backend
        .load_file()
        .expect_err("must reject newer schema version");
    assert!(
        error.to_string().contains("newer than supported version"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

fn set_fails_when_keyring_is_unavailable() -> Result<()> {
    let chaos_home = tempfile::tempdir().expect("tempdir");
    let keyring = Arc::new(MockKeyringStore::default());
    let account = compute_keyring_account(chaos_home.path());
    keyring.set_error(
        &account,
        KeyringError::Invalid("error".into(), "load".into()),
    );

    let backend = LocalSecretsBackend::new(chaos_home.path().to_path_buf(), keyring);
    let scope = SecretScope::Global;
    let name = SecretName::new("TEST_SECRET")?;
    let error = backend
        .set(&scope, &name, "secret-value")
        .expect_err("must fail when keyring load fails");
    assert!(
        error
            .to_string()
            .contains("failed to load secrets key from keyring"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

fn save_file_does_not_leave_temp_files() -> Result<()> {
    let chaos_home = tempfile::tempdir().expect("tempdir");
    let keyring = Arc::new(MockKeyringStore::default());
    let backend = LocalSecretsBackend::new(chaos_home.path().to_path_buf(), keyring);

    let scope = SecretScope::Global;
    let name = SecretName::new("TEST_SECRET")?;
    backend.set(&scope, &name, "one")?;
    backend.set(&scope, &name, "two")?;

    let secrets_dir = backend.secrets_dir();
    let entries = fs::read_dir(&secrets_dir)
        .with_context(|| format!("failed to read {}", secrets_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to enumerate {}", secrets_dir.display()))?;

    let filenames: Vec<String> = entries
        .into_iter()
        .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
        .collect();
    assert_eq!(filenames, vec![LOCAL_SECRETS_FILENAME.to_string()]);
    assert_eq!(backend.get(&scope, &name)?, Some("two".to_string()));
    Ok(())
}
