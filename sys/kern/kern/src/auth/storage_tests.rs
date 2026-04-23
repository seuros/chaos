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

#[tokio::test]
async fn file_storage_load_returns_auth_dot_json() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let storage = FileAuthStorage::new(chaos_home.path().to_path_buf());
    let auth_dot_json = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
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

#[test]
fn file_storage_delete_removes_auth_file() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let auth_dot_json = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
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

fn seed_keyring_and_fallback_auth_file_for_delete<F>(
    mock_keyring: &MockKeyringStore,
    chaos_home: &Path,
    compute_key: F,
) -> anyhow::Result<(String, PathBuf)>
where
    F: FnOnce() -> std::io::Result<String>,
{
    let key = compute_key()?;
    mock_keyring.save(KEYRING_SERVICE, &key, "{}")?;
    let auth_file = get_auth_file(chaos_home);
    std::fs::write(&auth_file, "stale")?;
    Ok((key, auth_file))
}

fn seed_keyring_with_auth<F>(
    mock_keyring: &MockKeyringStore,
    compute_key: F,
    auth: &AuthDotJson,
) -> anyhow::Result<()>
where
    F: FnOnce() -> std::io::Result<String>,
{
    let key = compute_key()?;
    let serialized = serde_json::to_string(&normalized(auth))?;
    mock_keyring.save(KEYRING_SERVICE, &key, &serialized)?;
    Ok(())
}

fn assert_keyring_saved_auth_and_removed_fallback(
    mock_keyring: &MockKeyringStore,
    key: &str,
    chaos_home: &Path,
    expected: &AuthDotJson,
) {
    let saved_value = mock_keyring
        .saved_value(key)
        .expect("keyring entry should exist");
    let expected_serialized =
        serde_json::to_string(&normalized(expected)).expect("serialize expected auth");
    assert_eq!(saved_value, expected_serialized);
    let auth_file = get_auth_file(chaos_home);
    assert!(
        !auth_file.exists(),
        "fallback auth.json should be removed after keyring save"
    );
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
    let storage = KeyringAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let expected = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                auth_mode: Some(AuthMode::ApiKey),
                api_key: Some("sk-test".to_string()),
                tokens: None,
                last_refresh: None,
            },
        )]
        .into_iter()
        .collect(),
    };
    seed_keyring_with_auth(
        &mock_keyring,
        || compute_store_key(chaos_home.path()),
        &expected,
    )?;

    let loaded = storage.load()?;
    assert_eq!(Some(normalized(&expected)), loaded);
    Ok(())
}

#[test]
fn keyring_auth_storage_compute_store_key_for_home_directory() -> anyhow::Result<()> {
    let chaos_home = PathBuf::from("~/.chaos");

    let key = compute_store_key(chaos_home.as_path())?;

    assert_eq!(key, "cli|b05defd32ba63b04");
    Ok(())
}

#[test]
fn keyring_auth_storage_save_persists_and_removes_fallback_file() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = KeyringAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let auth_file = get_auth_file(chaos_home.path());
    std::fs::write(&auth_file, "stale")?;
    let auth = AuthDotJson {
        providers: [(
            "openai".to_string(),
            ProviderAuthRecord {
                auth_mode: Some(AuthMode::Chatgpt),
                api_key: None,
                tokens: Some(TokenData {
                    id_token: Default::default(),
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

    let key = compute_store_key(chaos_home.path())?;
    assert_keyring_saved_auth_and_removed_fallback(&mock_keyring, &key, chaos_home.path(), &auth);
    Ok(())
}

#[test]
fn keyring_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = KeyringAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let (key, auth_file) =
        seed_keyring_and_fallback_auth_file_for_delete(&mock_keyring, chaos_home.path(), || {
            compute_store_key(chaos_home.path())
        })?;

    let removed = storage.delete()?;

    assert!(removed, "delete should report removal");
    assert!(
        !mock_keyring.contains(&key),
        "keyring entry should be removed"
    );
    assert!(
        !auth_file.exists(),
        "fallback auth.json should be removed after keyring delete"
    );
    Ok(())
}

#[test]
fn auto_auth_storage_load_prefers_keyring_value() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let keyring_auth = auth_with_prefix("keyring");
    seed_keyring_with_auth(
        &mock_keyring,
        || compute_store_key(chaos_home.path()),
        &keyring_auth,
    )?;

    let file_auth = auth_with_prefix("file");
    storage.file_storage.save(&file_auth)?;

    let loaded = storage.load()?;
    assert_eq!(loaded, Some(normalized(&keyring_auth)));
    Ok(())
}

#[test]
fn auto_auth_storage_load_uses_file_when_keyring_empty() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(chaos_home.path().to_path_buf(), Arc::new(mock_keyring));

    let expected = auth_with_prefix("file-only");
    storage.file_storage.save(&expected)?;

    let loaded = storage.load()?;
    assert_eq!(loaded, Some(normalized(&expected)));
    Ok(())
}

#[test]
fn auto_auth_storage_load_falls_back_when_keyring_errors() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let key = compute_store_key(chaos_home.path())?;
    mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "load".into()));

    let expected = auth_with_prefix("fallback");
    storage.file_storage.save(&expected)?;

    let loaded = storage.load()?;
    assert_eq!(loaded, Some(normalized(&expected)));
    Ok(())
}

#[test]
fn auto_auth_storage_save_prefers_keyring() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let key = compute_store_key(chaos_home.path())?;

    let stale = auth_with_prefix("stale");
    storage.file_storage.save(&stale)?;

    let expected = auth_with_prefix("to-save");
    storage.save(&expected)?;

    assert_keyring_saved_auth_and_removed_fallback(
        &mock_keyring,
        &key,
        chaos_home.path(),
        &expected,
    );
    Ok(())
}

#[test]
fn auto_auth_storage_save_falls_back_when_keyring_errors() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let key = compute_store_key(chaos_home.path())?;
    mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "save".into()));

    let auth = auth_with_prefix("fallback");
    storage.save(&auth)?;

    let auth_file = get_auth_file(chaos_home.path());
    assert!(
        auth_file.exists(),
        "fallback auth.json should be created when keyring save fails"
    );
    let saved = storage
        .file_storage
        .load()?
        .context("fallback auth should exist")?;
    assert_eq!(saved, normalized(&auth));
    assert!(
        mock_keyring.saved_value(&key).is_none(),
        "keyring should not contain value when save fails"
    );
    Ok(())
}

#[test]
fn auto_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
    let chaos_home = tempdir()?;
    let mock_keyring = MockKeyringStore::default();
    let storage = AutoAuthStorage::new(
        chaos_home.path().to_path_buf(),
        Arc::new(mock_keyring.clone()),
    );
    let (key, auth_file) =
        seed_keyring_and_fallback_auth_file_for_delete(&mock_keyring, chaos_home.path(), || {
            compute_store_key(chaos_home.path())
        })?;

    let removed = storage.delete()?;

    assert!(removed, "delete should report removal");
    assert!(
        !mock_keyring.contains(&key),
        "keyring entry should be removed"
    );
    assert!(
        !auth_file.exists(),
        "fallback auth.json should be removed after delete"
    );
    Ok(())
}
