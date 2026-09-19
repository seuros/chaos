use super::*;
use crate::auth::{AuthCredentialsStoreMode, login_with_provider_api_key};
use crate::reflex::configuration::{presets, resolve_api_key, save_backend, validate};
use chaos_sysctl::secrets;
use pretty_assertions::assert_eq;

fn auth(config: &Config) -> crate::AuthManager {
    crate::AuthManager::new(
        config.chaos_home.clone(),
        false,
        AuthCredentialsStoreMode::Ephemeral,
    )
}

#[tokio::test]
#[serial]
async fn setup_stores_only_a_reference_and_resolves_without_environment() {
    let _keyring = chaos_keyring::tests::MockKeyringStore::default();
    let (session, context) = make_session_and_context().await;
    let config = &context.config;
    let auth = auth(config);
    let (_, settings) = presets().into_iter().next().unwrap();
    let saved = save_backend(
        config,
        &auth,
        "typesafe",
        settings,
        Some("test-reflex-secret"),
    )
    .await
    .unwrap();
    let snapshot = crate::user_settings::snapshot(&config.chaos_home)
        .await
        .unwrap();
    let encoded = toml::to_string(&snapshot.settings).unwrap();
    assert!(encoded.contains("keyring:chaos-settings/"));
    assert!(!encoded.contains("test-reflex-secret"));
    assert!(!format!("{saved:?}").contains("test-reflex-secret"));
    assert!(!config.chaos_home.join("auth.json").exists());
    assert!(!config.chaos_home.join("config.toml").exists());
    let loaded: ReflexBackendSettings = snapshot.settings["reflex"]["typesafe"]
        .clone()
        .try_into()
        .unwrap();
    assert_eq!(loaded, saved);
    session.reload_user_config_layer().await;
    assert_eq!(
        session.get_config().await.reflex.get("typesafe"),
        Some(&saved)
    );
    assert!(
        context.config.reflex.is_empty(),
        "existing turn must keep its snapshot"
    );
    assert_eq!(
        resolve_api_key(config, &loaded, Some(&auth))
            .unwrap()
            .as_deref(),
        Some("test-reflex-secret")
    );
    let reloaded = session.get_config().await;
    let mut changed_host = saved.clone();
    changed_host.base_url = Some("https://different.example".into());
    assert!(
        save_backend(&reloaded, &auth, "typesafe", changed_host, None)
            .await
            .is_err()
    );
    let rotated = save_backend(
        config,
        &auth,
        "typesafe",
        saved.clone(),
        Some("rotated-secret"),
    )
    .await
    .unwrap();
    assert_ne!(rotated.api_key, saved.api_key);
    assert_eq!(
        resolve_api_key(config, &rotated, None).unwrap().as_deref(),
        Some("rotated-secret")
    );
    secrets::remove(rotated.api_key.as_deref().unwrap()).unwrap();
    assert!(resolve_api_key(config, &rotated, Some(&auth)).is_err());
}

#[tokio::test]
#[serial]
async fn a_failed_settings_write_does_not_replace_existing_credentials() {
    let _keyring = chaos_keyring::tests::MockKeyringStore::default();
    let home = tempfile::tempdir().unwrap();
    let (_, context) = make_session_and_context().await;
    let mut config = (*context.config).clone();
    config.chaos_home = home.path().to_path_buf();
    let config = &config;
    let auth = auth(config);
    let (_, settings) = presets().into_iter().next().unwrap();
    let saved = save_backend(config, &auth, "typesafe", settings, Some("original-key"))
        .await
        .unwrap();
    let before = crate::user_settings::snapshot(&config.chaos_home)
        .await
        .unwrap();
    let file = config.chaos_home.join("config.toml");
    std::fs::write(&file, "model = 'requires-migration'").unwrap();
    let err = save_backend(
        config,
        &auth,
        "typesafe",
        saved.clone(),
        Some("replacement-key"),
    )
    .await
    .unwrap_err();
    assert!(!err.to_string().contains("replacement-key"));
    assert_eq!(
        resolve_api_key(config, &saved, None).unwrap().as_deref(),
        Some("original-key")
    );
    std::fs::remove_file(file).unwrap();
    let after = crate::user_settings::snapshot(&config.chaos_home)
        .await
        .unwrap();
    assert_eq!(before.revision, after.revision);
    assert_eq!(before.settings, after.settings);
}

#[tokio::test]
#[serial]
async fn stored_provider_account_is_reused_only_for_its_origin() {
    let (_, context) = make_session_and_context().await;
    let mut config = (*context.config).clone();
    let (_, mut settings) = presets().into_iter().nth(1).unwrap();
    config.model_providers.insert(
        "reflex-test".into(),
        crate::ModelProviderInfo {
            name: "test".into(),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            ..config.model_provider.clone()
        },
    );
    login_with_provider_api_key(
        &config.chaos_home,
        "reflex-test",
        "stored-provider-key",
        AuthCredentialsStoreMode::Ephemeral,
    )
    .unwrap();
    settings.auth_provider = Some("reflex-test".into());
    let auth = auth(&config);
    assert_eq!(
        resolve_api_key(&config, &settings, Some(&auth))
            .unwrap()
            .as_deref(),
        Some("stored-provider-key")
    );
    let saved = save_backend(&config, &auth, "openrouter", settings.clone(), None)
        .await
        .unwrap();
    assert!(
        saved.api_key.is_none(),
        "reuse must not copy the account key"
    );
    settings.base_url = Some("https://different.example/api".into());
    assert!(resolve_api_key(&config, &settings, Some(&auth)).is_err());
    settings.base_url = saved.base_url;
    settings.env_key = Some(TEST_ENV_KEY.into());
    assert!(resolve_api_key(&config, &settings, Some(&auth)).is_err());
}

#[tokio::test]
#[serial]
async fn invalid_setup_is_rejected_without_persistence() {
    let (_, context) = make_session_and_context().await;
    let config = &context.config;
    let auth = auth(config);
    let (_, mut settings) = presets().into_iter().next().unwrap();
    let _env = EnvVarGuard::set("TYPESAFE_API_KEY", OsStr::new("must-not-be-used"));
    assert!(resolve_api_key(config, &settings, Some(&auth)).is_err());
    for base in [
        "http://example.com",
        "https://user:secret@example.com",
        "https://example.com?token=secret",
    ] {
        settings.base_url = Some(base.into());
        assert!(
            save_backend(config, &auth, "test", settings.clone(), Some("private-key"))
                .await
                .is_err()
        );
    }
    let (_, mut settings) = presets().into_iter().next().unwrap();
    settings.api_key = Some("literal-private-key".into());
    let err = validate(config, &settings).unwrap_err().to_string();
    assert!(!err.contains("literal-private-key"));
    assert!(!config.chaos_home.join("auth.json").exists());
    assert!(!config.chaos_home.join("config.toml").exists());
    let snapshot = crate::user_settings::snapshot(&config.chaos_home)
        .await
        .unwrap();
    assert!(snapshot.settings.get("reflex").is_none());
}
