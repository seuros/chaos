use super::*;
use chaos_keyring::{DefaultKeyringStore, KeyringStore};

// Keep native mock registration and shared-vault checks in one test: the
// keyring-core default store is process-global.
#[test]
fn credential_vault_lifecycle() -> anyhow::Result<()> {
    let _store = chaos_keyring::tests::MockKeyringStore::default();
    let home = tempfile::tempdir()?;
    let home = home.path();
    let original = serde_json::json!({
        "bearer_token":"private-token",
        "env":{"TOKEN":"private-env", "PATH":"/custom/bin"},
        "http_headers":{"X-Token":"private-header"},
        "nested":[{"api_key":"private-array-key"}],
        "instructions":"keyring:chaos-settings/not-a-reference-to-resolve"
    });
    let mut stored = original.clone();
    transform(home, &mut stored, true)?;
    let encoded = serde_json::to_string(&stored)?;
    for secret in [
        "private-token",
        "private-env",
        "private-header",
        "private-array-key",
    ] {
        assert!(!encoded.contains(secret));
    }
    let references = stored.clone();
    transform(home, &mut stored, true)?;
    assert_eq!(stored, references);
    transform(home, &mut stored, false)?;
    assert_eq!(stored, original);
    let reference = references["bearer_token"].as_str().unwrap();
    assert!(
        DefaultKeyringStore
            .load(SERVICE, reference.strip_prefix(PREFIX).unwrap())?
            .is_none()
    );
    assert_eq!(resolve(home, reference)?, "private-token");
    let other_home = tempfile::tempdir()?;
    assert!(resolve(other_home.path(), reference).is_err());
    assert!(externalize(home, "keyring:chaos-settings/not-a-uuid").is_err());
    remove(home, reference)?;
    assert!(resolve(home, reference).is_err());
    assert!(remove(home, "not-a-reference").is_err());

    // Legacy Keychain items are never read, even when the vault record is absent.
    let id = uuid::Uuid::new_v4().to_string();
    let legacy = format!("{PREFIX}{id}");
    DefaultKeyringStore.save(SERVICE, &id, "legacy-secret")?;
    let error = resolve(home, &legacy).unwrap_err().to_string();
    assert!(error.contains("re-enter it or restore the vault"));
    assert!(!error.contains("migrate-secrets"));
    assert_eq!(
        DefaultKeyringStore.load(SERVICE, &id)?.as_deref(),
        Some("legacy-secret")
    );
    LocalSecretsBackend::shared(home.to_path_buf())
        .save_credential(&credential_key(&id), "rotated")?;
    assert_eq!(resolve(home, &legacy)?, "rotated");
    remove(home, &legacy)?;
    assert!(
        resolve(home, &legacy).is_err(),
        "legacy Keychain credentials must not resurrect a deletion"
    );

    let ciphertext = std::fs::read(home.join("secrets/local.age"))?;
    assert!(!String::from_utf8_lossy(&ciphertext).contains("private-token"));
    Ok(())
}
