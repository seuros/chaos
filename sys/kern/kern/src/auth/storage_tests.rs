use super::*;
use crate::token_data::IdTokenInfo;
use anyhow::Context;
use jiff::Timestamp;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;

use chaos_keyring::tests::MockKeyringStore;
use keyring_core::Error as KeyringError;

#[allow(clippy::duplicate_mod)]
#[path = "../test_support/auth_fixtures.rs"]
mod auth_test_fixtures;

fn normalized(auth: &AuthDotJson) -> AuthDotJson {
    auth.normalized()
}

#[test]
fn normalized_auth_serialization_omits_legacy_openai_api_key_when_none() {
    let auth = AuthDotJson {
        providers: [(
            "zai-coding".to_string(),
            ProviderAuthRecord {
                credential_subject: Some("zai-test-subject".to_string()),
                auth_mode: Some(AuthMode::ApiKey),
                api_key: Some("test-key".to_string()),
                tokens: None,
                last_refresh: None,
            },
        )]
        .into_iter()
        .collect(),
    };

    let serialized = serde_json::to_value(auth.normalized()).expect("serialize normalized auth");
    let object = serialized
        .as_object()
        .expect("normalized auth should serialize to a json object");

    assert!(
        !object.contains_key("OPENAI_API_KEY"),
        "legacy OPENAI_API_KEY field should be omitted when empty"
    );
    assert_eq!(
        object
            .get("providers")
            .and_then(|providers| providers.get("zai-coding"))
            .and_then(|provider| provider.get("api_key"))
            .and_then(serde_json::Value::as_str),
        Some("test-key")
    );
}

#[test]
fn legacy_file_auth_gains_and_persists_stable_credential_subject() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let auth_file = get_auth_file(chaos_home.path());
    std::fs::write(
        &auth_file,
        r#"{
  "providers": {
    "legacy": {
      "api_key": "legacy-secret"
    }
  }
}"#,
    )?;
    let storage = FileAuthStorage::new(chaos_home.path().to_path_buf());

    let first = storage.load()?.context("legacy auth should load")?;
    let first_subject = first
        .provider_record("legacy")
        .and_then(|record| record.credential_subject)
        .context("legacy record should gain a subject")?;
    let second = storage.load()?.context("migrated auth should load")?;
    let second_subject = second
        .provider_record("legacy")
        .and_then(|record| record.credential_subject)
        .context("migrated record should retain its subject")?;
    let persisted = storage.try_read_auth_json(&auth_file)?;

    assert_eq!(first_subject, second_subject);
    assert_eq!(
        persisted
            .provider_record("legacy")
            .and_then(|record| record.credential_subject),
        Some(first_subject)
    );
    assert_eq!(
        persisted
            .provider_record("legacy")
            .and_then(|record| record.api_key)
            .as_deref(),
        Some("legacy-secret")
    );
    Ok(())
}

#[test]
fn duplicate_credential_subjects_produce_the_same_opaque_identity() {
    let record = |credential_subject: &str, api_key: &str| ProviderAuthRecord {
        credential_subject: Some(credential_subject.to_string()),
        auth_mode: Some(AuthMode::ApiKey),
        api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
    };
    let domain = "chaos/review/account/v1";

    let first = record("duplicate-local-subject", "first-secret")
        .credential_subject_fingerprint(domain)
        .expect("first fingerprint");
    let duplicate = record("duplicate-local-subject", "second-secret")
        .credential_subject_fingerprint(domain)
        .expect("duplicate fingerprint");
    let distinct = record("distinct-local-subject", "first-secret")
        .credential_subject_fingerprint(domain)
        .expect("distinct fingerprint");

    assert_eq!(first, duplicate);
    assert_ne!(first, distinct);
    assert!(!first.as_str().contains("first-secret"));
    assert!(!first.as_str().contains("duplicate-local-subject"));
}

#[test]
fn replacing_provider_credentials_preserves_local_subject() {
    let mut auth = AuthDotJson {
        providers: Default::default(),
    };
    let record = |api_key: &str| ProviderAuthRecord {
        credential_subject: None,
        auth_mode: Some(AuthMode::ApiKey),
        api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
    };

    auth.set_provider_record("provider", record("old-secret"));
    let before = auth
        .provider_record("provider")
        .and_then(|record| record.credential_subject)
        .expect("generated subject");
    let mut rotated = record("rotated-secret");
    rotated.credential_subject = Some("attempted-identity-reset".to_string());
    auth.set_provider_record("provider", rotated);
    let after = auth
        .provider_record("provider")
        .and_then(|record| record.credential_subject)
        .expect("preserved subject");

    assert_eq!(before, after);
    assert_eq!(
        auth.provider_record("provider")
            .and_then(|record| record.api_key)
            .as_deref(),
        Some("rotated-secret")
    );
}

#[tokio::test]
async fn file_storage_load_returns_auth_dot_json() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let storage = FileAuthStorage::new(chaos_home.path().to_path_buf());
    let auth_dot_json = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                credential_subject: Some("openai-load-subject".to_string()),
                auth_mode: Some(AuthMode::ApiKey),
                api_key: Some("test-key".to_string()),
                tokens: None,
                last_refresh: Some(Timestamp::now()),
            },
        )]
        .into_iter()
        .collect(),
    };

    storage
        .save(&auth_dot_json)
        .context("failed to save auth file")?;

    let loaded = storage.load().context("failed to load auth file")?;
    assert_eq!(Some(normalized(&auth_dot_json)), loaded);
    Ok(())
}

#[tokio::test]
async fn file_storage_save_persists_auth_dot_json() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let storage = FileAuthStorage::new(chaos_home.path().to_path_buf());
    let auth_dot_json = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                credential_subject: Some("openai-save-subject".to_string()),
                auth_mode: Some(AuthMode::ApiKey),
                api_key: Some("test-key".to_string()),
                tokens: None,
                last_refresh: Some(Timestamp::now()),
            },
        )]
        .into_iter()
        .collect(),
    };

    let file = get_auth_file(chaos_home.path());
    storage
        .save(&auth_dot_json)
        .context("failed to save auth file")?;

    let same_auth_dot_json = storage
        .try_read_auth_json(&file)
        .context("failed to read auth file after save")?;
    assert_eq!(normalized(&auth_dot_json), same_auth_dot_json);
    Ok(())
}

#[tokio::test]
async fn file_storage_save_is_atomic_and_leaves_no_temp_files() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let storage = FileAuthStorage::new(chaos_home.path().to_path_buf());

    let first = auth_with_prefix("first");
    storage.save(&first).context("save first")?;

    // Overwriting must replace the previous contents in place and must not
    // strand a temp file alongside auth.json.
    let second = auth_with_prefix("second");
    storage.save(&second).context("save second")?;

    let loaded = storage.load()?.context("auth should load")?;
    assert_eq!(loaded, normalized(&second));

    let leftovers: Vec<_> = std::fs::read_dir(chaos_home.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "auth.json")
        .collect();
    assert!(
        leftovers.is_empty(),
        "save should leave only auth.json, found: {leftovers:?}"
    );
    Ok(())
}

#[test]
fn file_storage_delete_removes_auth_file() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let auth_dot_json = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                credential_subject: Some("openai-delete-subject".to_string()),
                auth_mode: Some(AuthMode::ApiKey),
                api_key: Some("sk-test-key".to_string()),
                tokens: None,
                last_refresh: None,
            },
        )]
        .into_iter()
        .collect(),
    };
    let storage = create_auth_storage(dir.path().to_path_buf(), AuthCredentialsStoreMode::File);
    storage.save(&auth_dot_json)?;
    assert!(dir.path().join("auth.json").exists());
    let storage = FileAuthStorage::new(dir.path().to_path_buf());
    let removed = storage.delete()?;
    assert!(removed);
    assert!(!dir.path().join("auth.json").exists());
    Ok(())
}

#[test]
fn ephemeral_storage_save_load_delete_is_in_memory_only() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let storage = create_auth_storage(
        dir.path().to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    let auth_dot_json = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                credential_subject: Some("openai-ephemeral-subject".to_string()),
                auth_mode: Some(AuthMode::ApiKey),
                api_key: Some("sk-ephemeral".to_string()),
                tokens: None,
                last_refresh: Some(Timestamp::now()),
            },
        )]
        .into_iter()
        .collect(),
    };

    storage.save(&auth_dot_json)?;
    let loaded = storage.load()?;
    assert_eq!(Some(normalized(&auth_dot_json)), loaded);

    let removed = storage.delete()?;
    assert!(removed);
    let loaded = storage.load()?;
    assert_eq!(None, loaded);
    assert!(!get_auth_file(dir.path()).exists());
    Ok(())
}

fn id_token_with_prefix(prefix: &str) -> IdTokenInfo {
    auth_test_fixtures::id_token_from_payload(json!({
        "email": format!("{prefix}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_account_id": format!("{prefix}-account"),
        },
    }))
}

fn auth_with_prefix(prefix: &str) -> AuthDotJson {
    auth_test_fixtures::openai_auth(
        AuthMode::ApiKey,
        Some(&format!("{prefix}-api-key")),
        Some(TokenData {
            id_token: id_token_with_prefix(prefix),
            access_token: format!("{prefix}-access"),
            refresh_token: format!("{prefix}-refresh"),
            account_id: Some(format!("{prefix}-account-id")),
        }),
        None,
    )
}

#[test]
fn keyring_auth_storage_load_returns_deserialized_auth() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = VaultAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let expected = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                credential_subject: Some("openai-keyring-load-subject".to_string()),
                auth_mode: Some(AuthMode::ApiKey),
                api_key: Some("sk-test".to_string()),
                tokens: None,
                last_refresh: None,
            },
        )]
        .into_iter()
        .collect(),
    };
    storage.save(&expected)?;

    let loaded = storage.load()?;
    assert_eq!(Some(normalized(&expected)), loaded);
    Ok(())
}

#[test]
fn keyring_auth_storage_compute_store_key_for_home_directory() {
    let chaos_home = PathBuf::from("~/.chaos");

    let key = compute_store_key(chaos_home.as_path());

    assert_eq!(key, "cli|b05defd32ba63b04");
}

#[test]
fn keyring_auth_storage_save_persists_and_removes_fallback_file() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = VaultAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let auth_file = get_auth_file(chaos_home.path());
    std::fs::write(&auth_file, "stale")?;
    let auth = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                credential_subject: Some("openai-keyring-save-subject".to_string()),
                auth_mode: Some(AuthMode::Chatgpt),
                api_key: None,
                tokens: Some(TokenData {
                    id_token: id_token_with_prefix("vault-save"),
                    access_token: "access".to_string(),
                    refresh_token: "refresh".to_string(),
                    account_id: Some("account".to_string()),
                }),
                last_refresh: Some(Timestamp::now()),
            },
        )]
        .into_iter()
        .collect(),
    };

    storage.save(&auth)?;

    let key = compute_store_key(chaos_home.path());
    assert_eq!(storage.load()?, Some(normalized(&auth)));
    assert!(
        !mock_keyring.contains(&key),
        "provider tokens belong in the vault, not Keychain"
    );
    assert!(!auth_file.exists());
    Ok(())
}

#[test]
fn keyring_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = VaultAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    storage.save(&auth_with_prefix("delete"))?;
    let auth_file = get_auth_file(chaos_home.path());
    std::fs::write(&auth_file, "stale")?;

    let removed = storage.delete()?;

    assert!(removed, "delete should report removal");
    assert!(storage.load()?.is_none());
    assert!(
        !auth_file.exists(),
        "fallback auth.json should be removed after keyring delete"
    );
    Ok(())
}

#[test]
fn secure_auth_never_reads_legacy_keychain_or_plaintext_implicitly() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    FileAuthStorage::new(chaos_home.path().into()).save(&auth_with_prefix("file"))?;
    mock_keyring.save(KEYRING_SERVICE, &compute_store_key(chaos_home.path()), "{}")?;
    for mode in [
        AuthCredentialsStoreMode::Keyring,
        AuthCredentialsStoreMode::Auto,
    ] {
        let storage = create_auth_storage_with_keyring_store(
            chaos_home.path().into(),
            mode,
            Arc::new(mock_keyring.clone()),
        );
        assert_eq!(storage.load()?, None);
    }
    Ok(())
}

#[derive(Debug)]
struct DeniedKeyring;

impl KeyringStore for DeniedKeyring {
    fn load(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<String>, chaos_keyring::CredentialStoreError> {
        Err(chaos_keyring::CredentialStoreError::new(
            KeyringError::Invalid("test".into(), "denied".into()),
        ))
    }
    fn save(&self, _: &str, _: &str, _: &str) -> Result<(), chaos_keyring::CredentialStoreError> {
        panic!("must not write after denied unlock")
    }
    fn delete(&self, _: &str, _: &str) -> Result<bool, chaos_keyring::CredentialStoreError> {
        panic!("must not delete the master key")
    }
}

#[test]
fn secure_auth_never_writes_plaintext_when_keychain_is_denied() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    for mode in [
        AuthCredentialsStoreMode::Keyring,
        AuthCredentialsStoreMode::Auto,
    ] {
        let storage = create_auth_storage_with_keyring_store(
            chaos_home.path().into(),
            mode,
            Arc::new(DeniedKeyring),
        );
        assert!(storage.save(&auth_with_prefix("secret")).is_err());
        assert!(!get_auth_file(chaos_home.path()).exists());
        assert!(!chaos_home.path().join("secrets/local.age").exists());
    }
    Ok(())
}

#[test]
fn explicit_auth_migration_preserves_rotation_and_logout() -> anyhow::Result<()> {
    let home = tempdir()?;
    let keyring = Arc::new(MockKeyringStore::default());
    let storage = VaultAuthStorage::new(home.path().into(), keyring.clone());
    let legacy = normalized(&auth_with_prefix("legacy"));
    let key = compute_store_key(home.path());
    let serialized = serde_json::to_string(&legacy)?;
    keyring.save(KEYRING_SERVICE, &key, &serialized)?;
    assert!(storage.load()?.is_none());
    import_keyring_auth(home.path(), &storage.vault, keyring.as_ref())?;
    assert_eq!(storage.load()?, Some(legacy));
    assert_eq!(
        keyring.saved_value(&key).as_deref(),
        Some(serialized.as_str())
    );

    let rotated = normalized(&auth_with_prefix("rotated"));
    storage.save(&rotated)?;
    import_keyring_auth(home.path(), &storage.vault, keyring.as_ref())?;
    assert_eq!(storage.load()?, Some(rotated));
    storage.delete()?;
    import_keyring_auth(home.path(), &storage.vault, keyring.as_ref())?;
    assert!(storage.load()?.is_none());
    Ok(())
}
