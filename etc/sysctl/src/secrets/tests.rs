use super::*;

#[test]
fn credential_vault_lifecycle() -> anyhow::Result<()> {
    chaos_vault::testing::isolate();
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
    assert_eq!(resolve(home, reference)?, "private-token");
    let other_home = tempfile::tempdir()?;
    assert!(resolve(other_home.path(), reference).is_err());
    assert!(externalize(home, "keyring:chaos-settings/not-a-uuid").is_err());
    remove(home, reference)?;
    assert!(resolve(home, reference).is_err());
    assert!(remove(home, "not-a-reference").is_err());

    // A reference with no vault record asks to be re-entered.
    let id = uuid::Uuid::new_v4().to_string();
    let missing = format!("{PREFIX}{id}");
    let error = resolve(home, &missing).unwrap_err().to_string();
    assert!(error.contains("re-enter it or restore the vault"));
    LocalSecretsBackend::shared(home.to_path_buf())
        .save_credential(&credential_key(&id), "rotated")?;
    assert_eq!(resolve(home, &missing)?, "rotated");
    remove(home, &missing)?;
    assert!(resolve(home, &missing).is_err(), "a removal stays removed");

    let ciphertext = std::fs::read(home.join("secrets/local.age"))?;
    assert!(!String::from_utf8_lossy(&ciphertext).contains("private-token"));
    Ok(())
}
