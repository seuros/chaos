use super::*;
use crate::auth::AuthCredentialsStoreMode;
use crate::test_support::test_remote_model;
use chaos_ipc::openai_models::{ModelVisibility, ModelsResponse};
use core_test_support::responses::mount_models_once;
use tempfile::tempdir;
use wiremock::MockServer;

#[test]
fn model_selection_ids_preserve_arbitrary_account_ids_and_wire_slugs() {
    let choice = ModelSelection {
        provider_id: "account::one/\"two\"".into(),
        model: "vendor/model::version".into(),
    };
    assert_eq!(ModelSelection::from_id(&choice.id()), Some(choice));
    assert_eq!(ModelSelection::from_id("vendor/model"), None);
    assert_eq!(ModelSelection::from_id("[\"incomplete\"]"), None);
    assert_eq!(
        ModelSelection::wire_model("chaos:model:[\"account\",\"vendor/model\"]"),
        "vendor/model"
    );
}

#[test]
fn model_selection_matching_is_account_scoped_and_rejects_malformed_ids() {
    let first = ModelSelection::resolve("shared", "first").expect("bare model");
    let second = ModelSelection::resolve("shared", "second").expect("bare model");
    assert!(ModelSelection::same_model(&first.id(), "shared", "first"));
    assert!(ModelSelection::same_model(
        &first.id(),
        &first.id(),
        "second"
    ));
    assert!(!ModelSelection::same_model(
        &first.id(),
        &second.id(),
        "first"
    ));
    assert!(!ModelSelection::same_model(&second.id(), "shared", "first"));
    assert_eq!(ModelSelection::resolve(&first.id(), "second"), Some(first));
    assert_eq!(
        ModelSelection::resolve("chaos:model:invalid", "first"),
        None
    );
    assert!(!ModelSelection::same_model(
        "chaos:model:invalid",
        "chaos:model:invalid",
        "first"
    ));
}

#[test]
fn picker_catalog_filters_and_deduplicates_without_merging_accounts() {
    let preset = |provider: &str, visible: bool| {
        let mut preset: ModelPreset = test_remote_model("shared", ModelVisibility::List, 1).into();
        preset.id = ModelSelection::resolve("shared", provider)
            .expect("test model identity")
            .id();
        preset.show_in_picker = visible;
        preset
    };
    let first = preset("first", true);
    let second = preset("second", true);
    let mut alternate_encoding = first.clone();
    alternate_encoding.id = "chaos:model:[ \"first\", \"shared\" ]".into();
    let mut malformed = first.clone();
    malformed.id = "chaos:model:invalid".into();
    let catalog = ModelPickerCatalog::new(vec![
        preset("first", false),
        first.clone(),
        first.clone(),
        alternate_encoding,
        malformed,
        second.clone(),
        preset("hidden", false),
    ]);
    assert_eq!(catalog.models(), [first.clone(), second.clone()]);
    assert_eq!(catalog.find("shared", "first"), Some(&first));
    assert_eq!(catalog.find("shared", "second"), Some(&second));
    assert_eq!(catalog.find(&second.id, "first"), Some(&second));
    assert_eq!(catalog.find("shared", "hidden"), None);
    assert_eq!(catalog.find("chaos:model:invalid", "first"), None);

    let mut bridge = first;
    bridge.id = bridge.model.clone();
    let catalog = ModelPickerCatalog::new(vec![bridge.clone(), bridge.clone()]);
    assert_eq!(catalog.find(&second.id, "first"), None);
    assert_eq!(catalog.into_models(), [bridge], "bridge ids stay unchanged");
}

#[tokio::test]
async fn model_catalog_spans_accounts_but_resolution_keeps_metadata_and_bindings_isolated()
-> anyhow::Result<()> {
    let home = tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    let mut config = crate::config::ConfigBuilder::default()
        .chaos_home(home.path().to_path_buf())
        .build()
        .await?;
    let mount = chaos_vfs::MountConfig::sqlite_home(home.path());
    if chaos_vfs::mounted(&mount).is_none() {
        let backend = chaos_vfs::ChaosVfs::from_config(mount.clone()).await?;
        chaos_vfs::mount(mount, backend);
    }
    let first_server = MockServer::start().await;
    let second_server = MockServer::start().await;
    let provider = |url: String| -> crate::ModelProviderInfo {
        serde_json::from_value(serde_json::json!({
            "name": "OpenAI", "base_url": url, "wire_api": "responses",
            "experimental_bearer_token": "test-key", "request_max_retries": 0
        }))
        .expect("test provider")
    };
    let first = provider(first_server.uri());
    let second = provider(second_server.uri());
    let mut locked = second.clone();
    locked.experimental_bearer_token = None;
    locked.auth = Some(crate::ProviderAuthCapabilities {
        methods: vec![crate::ProviderAuthMethod::ApiKey],
    });
    let mut first_model = test_remote_model("shared-model", ModelVisibility::List, 1);
    first_model.context_window = Some(10_000);
    let mut second_model = first_model.clone();
    second_model.context_window = Some(20_000);
    mount_models_once(
        &first_server,
        ModelsResponse {
            models: vec![first_model],
        },
    )
    .await;
    mount_models_once(
        &second_server,
        ModelsResponse {
            models: vec![
                second_model,
                test_remote_model("hidden", ModelVisibility::Hide, 2),
            ],
        },
    )
    .await;
    config.model_provider_id = "first".into();
    config.model_provider = first.clone();
    config.model_providers = HashMap::from([
        ("first".into(), first.clone()),
        ("second".into(), second),
        ("same-endpoint-account".into(), first.clone()),
        ("locked".into(), locked),
    ]);
    // The live root binding can include an endpoint override absent from the registry.
    config
        .model_providers
        .insert("first".into(), provider("http://127.0.0.1:1".into()));
    let auth = crate::AuthManager::shared(
        home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
    );
    let manager = ModelsManager::new_with_provider_binding(
        home.path().to_path_buf(),
        auth,
        None,
        Default::default(),
        "first".into(),
        first,
    );
    manager
        .list_models(super::super::RefreshStrategy::OnlineIfUncached)
        .await;
    let catalog = manager
        .model_picker_catalog(
            &config.model_providers,
            "first",
            super::super::RefreshStrategy::OnlineIfUncached,
        )
        .await;
    let choices = catalog.models();
    assert_eq!(
        choices.len(),
        3,
        "hidden and logged-out choices are excluded"
    );
    let identities = choices
        .iter()
        .map(|preset| preset.id.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        identities.len(),
        3,
        "equal vendor slugs remain distinct per account"
    );
    assert!(choices.iter().all(|preset| preset.model == "shared-model"));
    let selected = ModelSelection {
        provider_id: "second".into(),
        model: "shared-model".into(),
    }
    .id();
    config.model = Some(selected.clone());
    manager.resolve_model_selection(&mut config).await?;
    assert_eq!(config.model_provider_id, "second");
    assert_eq!(
        config.model.as_deref(),
        Some("shared-model"),
        "never send a picker id on the wire"
    );
    let rebound = manager.for_config(&config).await;
    assert_eq!(
        rebound
            .get_model_info("shared-model", &config)
            .await
            .context_window,
        Some(20_000)
    );
    assert_eq!(
        manager.provider_id(),
        "first",
        "the host catalog stays bound to its own account"
    );
    let mut returning = config.clone();
    returning.model = Some(
        ModelSelection {
            provider_id: "first".into(),
            model: "shared-model".into(),
        }
        .id(),
    );
    manager.resolve_model_selection(&mut returning).await?;
    assert_eq!(
        &returning.model_provider,
        manager.provider(),
        "return to the actual bound endpoint"
    );
    let after = manager
        .model_picker_catalog(
            &config.model_providers,
            "second",
            super::super::RefreshStrategy::Offline,
        )
        .await;
    assert_eq!(
        after.models()[0].id,
        selected,
        "the selected account sorts first without stealing the host's catalog"
    );
    assert_eq!(
        manager
            .try_selected_model(&selected, "first")
            .map(|preset| preset.id),
        Some(selected.clone())
    );
    config.model_providers.remove("second");
    config.model_provider_id = "first".into();
    config.model_provider = config.model_providers["first"].clone();
    config.model = Some(selected);
    assert!(manager.resolve_model_selection(&mut config).await.is_err());
    assert_eq!(
        config.model_provider_id, "first",
        "rejected choices do not mutate the binding"
    );
    Ok(())
}
