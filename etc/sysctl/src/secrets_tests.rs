use super::*;

#[test]
fn references_round_trip_without_exposing_values_or_resolving_instructions() -> anyhow::Result<()> {
    let _store = chaos_keyring::tests::MockKeyringStore::default();
    let original = serde_json::json!({
        "bearer_token":"private-token",
        "env":{"TOKEN":"private-env"},
        "http_headers":{"X-Token":"private-header"},
        "nested":[{"api_key":"private-array-key"}],
        "instructions":"keyring:chaos-settings/not-a-reference-to-resolve"
    });
    let mut stored = original.clone();
    transform(&mut stored, true)?;
    let encoded = serde_json::to_string(&stored)?;
    assert!(!encoded.contains("private-token"));
    assert!(!encoded.contains("private-env"));
    assert!(!encoded.contains("private-header"));
    assert!(!encoded.contains("private-array-key"));
    let references = stored.clone();
    transform(&mut stored, true)?;
    assert_eq!(stored, references);
    transform(&mut stored, false)?;
    assert_eq!(stored, original);
    assert!(resolve("keyring:chaos-settings/00000000-0000-0000-0000-000000000000").is_err());
    assert_eq!(
        externalize("keyring:chaos-settings/00000000-0000-0000-0000-000000000000")?,
        "keyring:chaos-settings/00000000-0000-0000-0000-000000000000"
    );
    assert!(externalize("keyring:chaos-settings/not-a-uuid").is_err());
    Ok(())
}
