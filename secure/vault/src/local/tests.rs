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
        credentials: BTreeMap::new(),
    };
    backend.save_file(&file)?;
    backend.state.lock().unwrap().snapshot = None;

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

    let mut filenames: Vec<String> = entries
        .into_iter()
        .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
        .collect();
    filenames.sort();
    assert_eq!(
        filenames,
        vec![".lock".to_string(), LOCAL_SECRETS_FILENAME.to_string()]
    );
    assert_eq!(backend.get(&scope, &name)?, Some("two".to_string()));
    Ok(())
}

#[derive(Default)]
struct CountingKeyring {
    key: Mutex<Option<String>>,
    reads: std::sync::atomic::AtomicUsize,
    writes: std::sync::atomic::AtomicUsize,
    denied: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for CountingKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CountingKeyring")
    }
}

impl KeyringStore for CountingKeyring {
    fn load(
        &self,
        _: &str,
        _: &str,
    ) -> std::result::Result<Option<String>, chaos_keyring::CredentialStoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.denied.load(Ordering::SeqCst) {
            return Err(chaos_keyring::CredentialStoreError::new(
                KeyringError::Invalid("test".into(), "access denied".into()),
            ));
        }
        Ok(self.key.lock().unwrap().clone())
    }

    fn save(
        &self,
        _: &str,
        _: &str,
        value: &str,
    ) -> std::result::Result<(), chaos_keyring::CredentialStoreError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        *self.key.lock().unwrap() = Some(value.into());
        Ok(())
    }

    fn delete(
        &self,
        _: &str,
        _: &str,
    ) -> std::result::Result<bool, chaos_keyring::CredentialStoreError> {
        Ok(self.key.lock().unwrap().take().is_some())
    }
}

#[test]
fn unlock_once_observe_rotation_and_do_not_resurrect_deleted_credentials() -> Result<()> {
    let home = tempfile::tempdir()?;
    let keyring = Arc::new(CountingKeyring::default());
    let writer = LocalSecretsBackend::new(home.path().into(), keyring.clone());
    writer.save_credential("auth", "first")?;
    keyring.reads.store(0, Ordering::SeqCst);

    let reader = LocalSecretsBackend::new(home.path().into(), keyring.clone());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                assert_eq!(
                    reader.load_credential("auth").unwrap().as_deref(),
                    Some("first")
                );
            });
        }
    });
    assert_eq!(keyring.reads.load(Ordering::SeqCst), 1);
    keyring.denied.store(true, Ordering::SeqCst);
    writer.save_credential("auth", "rotated")?;
    assert_eq!(reader.load_credential("auth")?.as_deref(), Some("rotated"));
    writer.delete_credential("auth")?;
    assert_eq!(reader.load_credential("auth")?, None);
    reader.import_credentials(&[("auth".into(), "old".into())].into_iter().collect())?;
    assert_eq!(reader.load_credential("auth")?, None);
    let name = SecretName::new("TEST")?;
    reader.set(&SecretScope::Global, &name, "named-secret")?;
    assert_eq!(
        reader.list(None)?.len(),
        1,
        "internal credentials are not listed"
    );
    assert_eq!(keyring.reads.load(Ordering::SeqCst), 1);
    assert!(!format!("{reader:?}").contains("named-secret"));
    Ok(())
}

#[test]
fn missing_or_denied_unlock_key_never_replaces_an_existing_key_or_file() -> Result<()> {
    let home = tempfile::tempdir()?;
    let keyring = Arc::new(CountingKeyring::default());
    let writer = LocalSecretsBackend::new(home.path().into(), keyring.clone());
    writer.save_credential("auth", "secret")?;
    let ciphertext = fs::read(writer.secrets_path())?;
    let reader = LocalSecretsBackend::new(home.path().into(), keyring.clone());
    keyring.denied.store(true, Ordering::SeqCst);
    assert!(reader.load_credential("auth").is_err());
    keyring.denied.store(false, Ordering::SeqCst);
    assert_eq!(reader.load_credential("auth")?.as_deref(), Some("secret"));
    *keyring.key.lock().unwrap() = None;
    let locked = LocalSecretsBackend::new(home.path().into(), keyring.clone());
    let writes = keyring.writes.load(Ordering::SeqCst);
    assert!(locked.load_credential("auth").is_err());
    assert!(locked.save_credential("auth", "replacement").is_err());
    assert_eq!(keyring.writes.load(Ordering::SeqCst), writes);
    assert_eq!(fs::read(writer.secrets_path())?, ciphertext);
    Ok(())
}

#[test]
fn independent_writers_preserve_each_others_changes() -> Result<()> {
    let home = tempfile::tempdir()?;
    let keyring = Arc::new(CountingKeyring::default());
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for key in ["first", "second"] {
            let store = LocalSecretsBackend::new(home.path().into(), keyring.clone());
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                store.save_credential(key, "secret").unwrap();
            });
        }
    });
    let reader = LocalSecretsBackend::new(home.path().into(), keyring.clone());
    assert_eq!(reader.load_credential("first")?.as_deref(), Some("secret"));
    assert_eq!(reader.load_credential("second")?.as_deref(), Some("secret"));
    assert_eq!(keyring.writes.load(Ordering::SeqCst), 1);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(reader.secrets_path())?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

#[test]
fn independent_callers_share_only_their_home_unlock_state() -> Result<()> {
    let first = tempfile::tempdir()?;
    let second = tempfile::tempdir()?;
    let a = LocalSecretsBackend::shared(first.path().into());
    let b = LocalSecretsBackend::shared(first.path().join("."));
    let c = LocalSecretsBackend::shared(second.path().into());
    assert!(Arc::ptr_eq(&a.state, &b.state));
    assert!(!Arc::ptr_eq(&a.state, &c.state));
    assert!(a.load_credential("missing")?.is_none());
    assert!(
        fs::read_dir(first.path())?.next().is_none(),
        "inspection must not create a vault"
    );
    Ok(())
}

#[test]
fn version_one_named_secrets_survive_the_first_credential_write() -> Result<()> {
    let home = tempfile::tempdir()?;
    let keyring = Arc::new(CountingKeyring::default());
    let store = LocalSecretsBackend::new(home.path().into(), keyring);
    let _lock = store.lock_file()?;
    let passphrase = store.passphrase(&mut VaultState::default(), true)?;
    let legacy = br#"{"version":1,"secrets":{"global/TOKEN":"named-secret"}}"#;
    write_file_atomically(
        &store.secrets_path(),
        &encrypt_with_passphrase(legacy, &passphrase)?,
    )?;
    drop(_lock);
    assert_eq!(
        store
            .get(&SecretScope::Global, &SecretName::new("TOKEN")?)?
            .as_deref(),
        Some("named-secret")
    );
    store.save_credential("auth", "provider-secret")?;
    let file = store.load_file()?;
    assert_eq!(file.version, 2);
    assert_eq!(file.secrets["global/TOKEN"], "named-secret");
    assert_eq!(file.credentials["auth"].as_deref(), Some("provider-secret"));
    Ok(())
}

#[test]
fn corrupted_ciphertext_never_returns_the_cached_snapshot() -> Result<()> {
    let home = tempfile::tempdir()?;
    let store = LocalSecretsBackend::new(home.path().into(), Arc::new(CountingKeyring::default()));
    store.save_credential("auth", "secret")?;
    assert_eq!(store.load_credential("auth")?.as_deref(), Some("secret"));
    fs::write(store.secrets_path(), b"corrupt")?;
    assert!(store.load_credential("auth").is_err());
    assert!(store.save_credential("auth", "replacement").is_err());
    assert_eq!(fs::read(store.secrets_path())?, b"corrupt");
    Ok(())
}
