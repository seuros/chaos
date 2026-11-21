use super::*;
use crate::testing::MemorySource;
use crate::testing::sources;
use custody_machines::SourceSpec;
use custody_machines::stores::Sighting;
use custody_machines::stores::Source;
use custody_machines::stores::SourceError;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;

fn memory_vault(home: &Path) -> (MemorySource, LocalSecretsBackend) {
    let key = MemorySource::durable("test keyring");
    let backend = LocalSecretsBackend::new(home.to_path_buf(), sources(&key));
    (key, backend)
}

#[test]
fn local_suite() {
    load_file_rejects_newer_schema_versions().expect("load_file_rejects_newer_schema_versions");
    set_fails_with_the_cause_when_no_key_source_answers()
        .expect("set_fails_with_the_cause_when_no_key_source_answers");
    save_file_does_not_leave_temp_files().expect("save_file_does_not_leave_temp_files");
}

fn load_file_rejects_newer_schema_versions() -> Result<()> {
    let chaos_home = tempfile::tempdir().expect("tempdir");
    let (_key, backend) = memory_vault(chaos_home.path());

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

fn set_fails_with_the_cause_when_no_key_source_answers() -> Result<()> {
    let chaos_home = tempfile::tempdir().expect("tempdir");
    let (key, backend) = memory_vault(chaos_home.path());
    key.set_unreachable(true);
    let scope = SecretScope::Global;
    let name = SecretName::new("TEST_SECRET")?;
    let error = backend
        .set(&scope, &name, "secret-value")
        .expect_err("must fail when no key source answers")
        .to_string();
    assert!(
        error.contains("cannot unlock the credential vault")
            && error.contains("test keyring: unreachable (unreachable for the test)"),
        "unexpected error: {error}"
    );
    Ok(())
}

fn save_file_does_not_leave_temp_files() -> Result<()> {
    let chaos_home = tempfile::tempdir().expect("tempdir");
    let (_key, backend) = memory_vault(chaos_home.path());

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

#[derive(Clone)]
struct CountingKeyring {
    key: MemorySource,
    reads: Arc<AtomicUsize>,
}

impl CountingKeyring {
    fn new() -> Self {
        Self {
            key: MemorySource::durable("counting keyring"),
            reads: Arc::default(),
        }
    }

    fn backend(&self, home: &Path) -> LocalSecretsBackend {
        let mut ladder = sources(&self.key);
        ladder[0] = Box::new(self.clone());
        LocalSecretsBackend::new(home.to_path_buf(), ladder)
    }
}

impl Source for CountingKeyring {
    fn name(&self) -> &str {
        self.key.name()
    }

    fn spec(&self) -> SourceSpec {
        self.key.spec()
    }

    fn read(&self) -> Sighting {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.key.read()
    }

    fn write(&self, key: &[u8]) -> std::result::Result<(), SourceError> {
        self.key.write(key)
    }
}

#[test]
fn unlock_once_observe_rotation_and_deletion() -> Result<()> {
    let home = tempfile::tempdir()?;
    let keyring = CountingKeyring::new();
    let writer = keyring.backend(home.path());
    writer.save_credential("auth", "first")?;
    keyring.reads.store(0, Ordering::SeqCst);

    let reader = keyring.backend(home.path());
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
    keyring.key.set_unreachable(true);
    writer.save_credential("auth", "rotated")?;
    assert_eq!(reader.load_credential("auth")?.as_deref(), Some("rotated"));
    writer.delete_credential("auth")?;
    assert_eq!(reader.load_credential("auth")?, None);
    assert!(!reader.delete_credential("auth")?);
    let name = SecretName::new("TEST")?;
    reader.set(&SecretScope::Global, &name, "named-secret")?;
    assert_eq!(
        reader.list(None)?.len(),
        1,
        "internal credentials are not listed"
    );
    let surveyed = keyring.reads.load(Ordering::SeqCst);
    assert!(reader.load_credential("auth")?.is_none());
    assert_eq!(
        keyring.reads.load(Ordering::SeqCst),
        surveyed,
        "reads never unlock again"
    );
    assert!(!format!("{reader:?}").contains("named-secret"));
    Ok(())
}

#[test]
fn missing_or_denied_unlock_key_never_replaces_an_existing_key_or_file() -> Result<()> {
    let home = tempfile::tempdir()?;
    let keyring = CountingKeyring::new();
    let writer = keyring.backend(home.path());
    writer.save_credential("auth", "secret")?;
    let ciphertext = fs::read(writer.secrets_path())?;
    let reader = keyring.backend(home.path());
    keyring.key.set_unreachable(true);
    assert!(reader.load_credential("auth").is_err());
    keyring.key.set_unreachable(false);
    assert_eq!(reader.load_credential("auth")?.as_deref(), Some("secret"));
    keyring.key.wipe();
    let locked = keyring.backend(home.path());
    let writes = keyring.key.writes();
    assert!(locked.load_credential("auth").is_err());
    assert!(locked.save_credential("auth", "replacement").is_err());
    assert_eq!(keyring.key.writes(), writes);
    assert_eq!(fs::read(writer.secrets_path())?, ciphertext);
    Ok(())
}

#[test]
fn independent_writers_preserve_each_others_changes() -> Result<()> {
    let home = tempfile::tempdir()?;
    let keyring = CountingKeyring::new();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for key in ["first", "second"] {
            let store = keyring.backend(home.path());
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                store.save_credential(key, "secret").unwrap();
            });
        }
    });
    let reader = keyring.backend(home.path());
    assert_eq!(reader.load_credential("first")?.as_deref(), Some("secret"));
    assert_eq!(reader.load_credential("second")?.as_deref(), Some("secret"));
    assert_eq!(keyring.key.writes(), 1);
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
    let keyring = CountingKeyring::new();
    let store = keyring.backend(home.path());
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
    let (_key, store) = memory_vault(home.path());
    store.save_credential("auth", "secret")?;
    assert_eq!(store.load_credential("auth")?.as_deref(), Some("secret"));
    fs::write(store.secrets_path(), b"corrupt")?;
    assert!(store.load_credential("auth").is_err());
    assert!(store.save_credential("auth", "replacement").is_err());
    assert_eq!(fs::read(store.secrets_path())?, b"corrupt");
    Ok(())
}

fn cache_ladder(durable: &MemorySource, cache: &MemorySource) -> VaultSources {
    std::array::from_fn(|index| -> Box<dyn Source> {
        if index == 0 {
            Box::new(durable.clone())
        } else if index == LADDER_LAST {
            Box::new(cache.clone())
        } else {
            Box::new(MemorySource::provisioned(format!("unused source {index}")))
        }
    })
}

const LADDER_LAST: usize = crate::custody::LADDER - 1;

#[test]
fn a_key_left_only_in_the_kernel_keyring_is_rescued_into_durable_storage() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (old, writer) = memory_vault(home.path());
    writer.save_credential("auth", "secret")?;
    let durable = MemorySource::durable("key file");
    let cache = MemorySource::cache("kernel keyring");
    cache.put(&old.held().expect("minted key"));
    let reader = LocalSecretsBackend::new(home.path().into(), cache_ladder(&durable, &cache));
    assert_eq!(reader.load_credential("auth")?.as_deref(), Some("secret"));
    assert_eq!(durable.held(), old.held(), "rescued before the next reboot");
    reader.save_credential("auth", "rotated")?;
    Ok(())
}

#[test]
fn a_key_that_dies_on_reboot_opens_the_vault_but_seals_nothing() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (old, writer) = memory_vault(home.path());
    writer.save_credential("auth", "secret")?;
    let ciphertext = fs::read(writer.secrets_path())?;
    let durable = MemorySource::durable("key file");
    durable.refuse_writes(true);
    let cache = MemorySource::cache("kernel keyring");
    cache.put(&old.held().expect("minted key"));
    let reader = LocalSecretsBackend::new(home.path().into(), cache_ladder(&durable, &cache));
    assert_eq!(reader.load_credential("auth")?.as_deref(), Some("secret"));
    let error = reader
        .save_credential("auth", "rotated")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("refusing to write the credential vault"),
        "{error}"
    );
    assert!(error.contains("dies with the next reboot"), "{error}");
    assert_eq!(fs::read(writer.secrets_path())?, ciphertext);
    Ok(())
}

#[test]
fn writes_stop_when_the_durable_copy_disappears_and_resume_when_it_returns() -> Result<()> {
    let home = tempfile::tempdir()?;
    let durable = MemorySource::durable("key file");
    let cache = MemorySource::cache("kernel keyring");
    let store = LocalSecretsBackend::new(home.path().into(), cache_ladder(&durable, &cache));
    store.save_credential("auth", "first")?;
    let key = durable.held().expect("minted key");
    durable.wipe();
    durable.refuse_writes(true);
    let error = store
        .save_credential("auth", "second")
        .unwrap_err()
        .to_string();
    assert!(error.contains("dies with the next reboot"), "{error}");
    durable.refuse_writes(false);
    store.save_credential("auth", "third")?;
    assert_eq!(
        durable.held(),
        Some(key),
        "the key went back to durable storage"
    );
    assert_eq!(store.load_credential("auth")?.as_deref(), Some("third"));
    Ok(())
}

#[test]
fn a_failed_recheck_keeps_the_earlier_refusal() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (old, writer) = memory_vault(home.path());
    writer.save_credential("auth", "secret")?;
    let durable = MemorySource::durable("key file");
    durable.refuse_writes(true);
    let cache = MemorySource::cache("kernel keyring");
    cache.put(&old.held().expect("minted key"));
    let reader = LocalSecretsBackend::new(home.path().into(), cache_ladder(&durable, &cache));
    assert_eq!(reader.load_credential("auth")?.as_deref(), Some("secret"));
    cache.wipe();
    assert!(reader.save_credential("auth", "lost").is_err());
    durable.set_unreachable(true);
    let error = reader
        .save_credential("auth", "silent")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("refusing to write the credential vault"),
        "{error}"
    );
    Ok(())
}

#[test]
fn a_recreated_vault_puts_its_key_back_in_durable_storage() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (key, store) = memory_vault(home.path());
    store.save_credential("auth", "first")?;
    key.wipe();
    fs::remove_file(store.secrets_path())?;
    store.save_credential("auth", "second")?;
    assert!(
        key.held().is_some(),
        "the recreated vault's key is stored durably"
    );
    let reopened = LocalSecretsBackend::new(home.path().into(), sources(&key));
    assert_eq!(reopened.load_credential("auth")?.as_deref(), Some("second"));
    Ok(())
}

#[test]
fn a_conclusive_loss_is_not_forgotten_when_the_next_survey_is_silent() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (key, store) = memory_vault(home.path());
    store.save_credential("auth", "first")?;
    key.wipe();
    key.refuse_writes(true);
    assert!(store.save_credential("auth", "lost").is_err());
    key.set_unreachable(true);
    let error = store
        .save_credential("auth", "silent")
        .unwrap_err()
        .to_string();
    assert!(error.contains("no source holds its key"), "{error}");
    Ok(())
}
