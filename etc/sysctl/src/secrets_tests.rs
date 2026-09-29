use super::*;

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
    migrate_references(home, &references)?; // Never interpret instructions as references.
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

    // Runtime never consults a legacy item. Only explicit migration may read it.
    let id = uuid::Uuid::new_v4().to_string();
    let legacy = format!("{PREFIX}{id}");
    DefaultKeyringStore.save(SERVICE, &id, "legacy-secret")?;
    assert!(resolve(home, &legacy).is_err());
    let config = serde_json::json!({"api_key": legacy});
    migrate_references(home, &config)?;
    assert_eq!(resolve(home, &legacy)?, "legacy-secret");
    // Preserve the source for recovery, but never overwrite a rotation on retry.
    assert_eq!(
        DefaultKeyringStore.load(SERVICE, &id)?.as_deref(),
        Some("legacy-secret")
    );
    LocalSecretsBackend::shared(home.to_path_buf())
        .save_credential(&credential_key(&id), "rotated")?;
    migrate_references(home, &config)?;
    assert_eq!(resolve(home, &legacy)?, "rotated");
    remove(home, &legacy)?;
    migrate_references(home, &config)?;
    assert!(
        resolve(home, &legacy).is_err(),
        "migration must not resurrect a deletion"
    );

    let missing = format!("{PREFIX}{}", uuid::Uuid::new_v4());
    let available_id = uuid::Uuid::new_v4().to_string();
    DefaultKeyringStore.save(SERVICE, &available_id, "not-yet-imported")?;
    let available = format!("{PREFIX}{available_id}");
    assert!(
        migrate_references(
            home,
            &serde_json::json!({
                "api_key": available, "bearer_token": missing,
            })
        )
        .is_err()
    );
    assert!(
        resolve(home, &available).is_err(),
        "failed batch must not be partially persisted"
    );

    let ciphertext = std::fs::read(home.join("secrets/local.age"))?;
    assert!(!String::from_utf8_lossy(&ciphertext).contains("private-token"));
    Ok(())
}
