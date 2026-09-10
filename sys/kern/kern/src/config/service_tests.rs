use super::*;
use anyhow::Result;
use chaos_ipc::api::AppConfig;
use chaos_ipc::api::AppToolApproval;
use chaos_ipc::api::AppsConfig;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn inspection_does_not_expose_resolved_bootstrap_values() -> Result<()> {
    let tmp = tempdir()?;
    std::fs::write(
        tmp.path().join(CONFIG_TOML_FILE),
        "egress_url = 'https://user:private-connection-secret@example.com/egress'\n",
    )?;
    let response = ConfigService::new_with_defaults(tmp.path().to_path_buf())
        .read(ConfigReadParams {
            include_layers: true,
            cwd: None,
        })
        .await?;
    let encoded = serde_json::to_string(&response)?;
    assert!(!encoded.contains("private-connection-secret"));
    assert!(!response.config.additional.contains_key("egress_url"));
    Ok(())
}

#[test]
fn toml_value_to_item_handles_nested_config_tables() {
    let config = r#"
[mcp_servers.docs]
command = "docs-server"

[mcp_servers.docs.http_headers]
X-Doc = "42"
"#;

    let value: TomlValue = toml::from_str(config).expect("parse config example");
    let item = toml_value_to_item(&value).expect("convert to toml_edit item");

    let root = item.as_table().expect("root table");
    assert!(!root.is_implicit(), "root table should be explicit");

    let mcp_servers = root
        .get("mcp_servers")
        .and_then(TomlItem::as_table)
        .expect("mcp_servers table");
    assert!(
        !mcp_servers.is_implicit(),
        "mcp_servers table should be explicit"
    );

    let docs = mcp_servers
        .get("docs")
        .and_then(TomlItem::as_table)
        .expect("docs table");
    assert_eq!(
        docs.get("command")
            .and_then(TomlItem::as_value)
            .and_then(toml_edit::Value::as_str),
        Some("docs-server")
    );

    let http_headers = docs
        .get("http_headers")
        .and_then(TomlItem::as_table)
        .expect("http_headers table");
    assert_eq!(
        http_headers
            .get("X-Doc")
            .and_then(TomlItem::as_value)
            .and_then(toml_edit::Value::as_str),
        Some("42")
    );
}

#[tokio::test]
async fn write_value_leaves_bootstrap_unchanged() -> Result<()> {
    let tmp = tempdir().expect("tempdir");
    let original = r#"# Chaos user configuration
model = "serpent"
approval_policy = "interactive"

[notice]
# Preserve this comment
hide_full_access_warning = true
"#;
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), original)?;
    crate::user_settings::migrate(tmp.path(), false).await?;
    let bootstrap = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE))?;

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "notice.hide_rate_limit_model_nudge".to_string(),
            value: serde_json::json!(true),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("write succeeds");

    let updated = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    assert_eq!(updated, bootstrap);
    let settings = crate::user_settings::snapshot(tmp.path()).await?.settings;
    assert_eq!(
        settings["notice"]["hide_full_access_warning"].as_bool(),
        Some(true)
    );
    assert_eq!(
        settings["notice"]["hide_rate_limit_model_nudge"].as_bool(),
        Some(true)
    );
    Ok(())
}

#[tokio::test]
async fn write_value_supports_nested_app_paths() -> Result<()> {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), "")?;

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "apps".to_string(),
            value: serde_json::json!({
                "app1": {
                    "enabled": false,
                },
            }),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("write apps succeeds");

    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "apps.app1.default_tools_approval_mode".to_string(),
            value: serde_json::json!("prompt"),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("write apps.app1.default_tools_approval_mode succeeds");

    let read = service
        .read(ConfigReadParams {
            include_layers: false,
            cwd: None,
        })
        .await
        .expect("config read succeeds");

    assert_eq!(
        read.config.apps,
        Some(AppsConfig {
            default: None,
            apps: std::collections::HashMap::from([(
                "app1".to_string(),
                AppConfig {
                    enabled: false,
                    destructive_enabled: None,
                    open_world_enabled: None,
                    default_tools_approval_mode: Some(AppToolApproval::Prompt),
                    default_tools_enabled: None,
                    tools: None,
                },
            )]),
        })
    );

    Ok(())
}

#[tokio::test]
async fn read_includes_origins_and_layers() {
    let tmp = tempdir().expect("tempdir");
    let user_path = tmp.path().join(CONFIG_TOML_FILE);
    std::fs::write(&user_path, "model = \"user\"").unwrap();
    crate::user_settings::migrate(tmp.path(), false)
        .await
        .unwrap();

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());

    let response = service
        .read(ConfigReadParams {
            include_layers: true,
            cwd: None,
        })
        .await
        .expect("response");

    assert_eq!(response.config.model.as_deref(), Some("user"));

    assert_eq!(
        response.origins.get("model").expect("origin").name,
        ConfigLayerSource::UserDatabase { revision: 1 },
    );
    let layers = response.layers.expect("layers present");
    assert_eq!(layers.len(), 3, "database, bootstrap, system");
    assert_eq!(
        layers.first().unwrap().name,
        ConfigLayerSource::UserDatabase { revision: 1 }
    );
    assert!(matches!(
        layers.get(2).unwrap().name,
        ConfigLayerSource::System { .. }
    ));
}

#[tokio::test]
async fn version_conflict_rejected() {
    let tmp = tempdir().expect("tempdir");
    let user_path = tmp.path().join(CONFIG_TOML_FILE);
    std::fs::write(&user_path, "model = \"user\"").unwrap();
    crate::user_settings::migrate(tmp.path(), false)
        .await
        .unwrap();

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    let error = service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "model".to_string(),
            value: serde_json::json!("serpent"),
            merge_strategy: MergeStrategy::Replace,
            expected_version: Some("sha256:bogus".to_string()),
        })
        .await
        .expect_err("should fail");

    assert_eq!(
        error.write_error_code(),
        Some(ConfigWriteErrorCode::ConfigVersionConflict)
    );
}

#[tokio::test]
async fn write_value_defaults_to_database() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), "").unwrap();

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    service
        .write_value(ConfigValueWriteParams {
            file_path: None,
            key_path: "model".to_string(),
            value: serde_json::json!("gordon"),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("write succeeds");

    let contents = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    assert!(contents.is_empty(), "bootstrap must not be rewritten");
    assert_eq!(
        crate::user_settings::snapshot(tmp.path())
            .await
            .unwrap()
            .settings["model"]
            .as_str(),
        Some("gordon")
    );
}

#[tokio::test]
async fn invalid_user_value_rejected() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), "model = \"user\"").unwrap();
    crate::user_settings::migrate(tmp.path(), false)
        .await
        .unwrap();

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());

    let error = service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "approval_policy".to_string(),
            value: serde_json::json!("bogus"),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect_err("should fail validation");

    assert_eq!(
        error.write_error_code(),
        Some(ConfigWriteErrorCode::ConfigValidationError)
    );

    assert_eq!(
        crate::user_settings::snapshot(tmp.path())
            .await
            .unwrap()
            .settings["model"]
            .as_str(),
        Some("user")
    );
}

#[tokio::test]
async fn read_reports_session_flags_override_user() {
    let tmp = tempdir().expect("tempdir");
    let user_path = tmp.path().join(CONFIG_TOML_FILE);
    std::fs::write(&user_path, "model = \"user\"").unwrap();
    crate::user_settings::migrate(tmp.path(), false)
        .await
        .unwrap();

    let cli_overrides = vec![(
        "model".to_string(),
        TomlValue::String("session".to_string()),
    )];

    let service = ConfigService::new(
        tmp.path().to_path_buf(),
        cli_overrides,
        LoaderOverrides::default(),
    );

    let response = service
        .read(ConfigReadParams {
            include_layers: true,
            cwd: None,
        })
        .await
        .expect("response");

    assert_eq!(response.config.model.as_deref(), Some("session"));
    assert_eq!(
        response.origins.get("model").expect("origin").name,
        ConfigLayerSource::SessionFlags,
    );
    let layers = response.layers.expect("layers");
    assert_eq!(
        layers.first().unwrap().name,
        ConfigLayerSource::SessionFlags
    );
    assert_eq!(
        layers.get(1).unwrap().name,
        ConfigLayerSource::UserDatabase { revision: 1 }
    );
}

#[tokio::test]
async fn upsert_merges_tables_replace_overwrites() -> Result<()> {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join(CONFIG_TOML_FILE);
    let base = r#"[model_providers.linear]
name = "linear"
base_url = "https://linear.example"
env_key = "TOKEN"

[model_providers.linear.env_http_headers]
existing = "keep"

[model_providers.linear.query_params]
alpha = "a"
"#;

    let overlay = serde_json::json!({
        "env_key": "NEW_TOKEN",
        "query_params": {
            "alpha": "updated",
            "beta": "b"
        },
        "name": "linear",
        "base_url": "https://linear.example"
    });

    std::fs::write(&path, base)?;
    crate::user_settings::migrate(tmp.path(), false).await?;

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(path.display().to_string()),
            key_path: "model_providers.linear".to_string(),
            value: overlay.clone(),
            merge_strategy: MergeStrategy::Upsert,
            expected_version: None,
        })
        .await
        .expect("upsert succeeds");

    let upserted = crate::user_settings::snapshot(tmp.path()).await?.settings;
    let expected_upsert: TomlValue = toml::from_str(
        r#"[model_providers.linear]
env_key = "NEW_TOKEN"
name = "linear"
base_url = "https://linear.example"

[model_providers.linear.env_http_headers]
existing = "keep"

[model_providers.linear.query_params]
alpha = "updated"
beta = "b"
"#,
    )?;
    assert_eq!(upserted, expected_upsert);

    let runtime = crate::user_settings::open(tmp.path()).await?;
    let snapshot = runtime.settings_snapshot().await?;
    runtime
        .commit_settings(
            snapshot.revision,
            &serde_json::to_value(toml::from_str::<TomlValue>(base)?)?,
            None,
        )
        .await?;

    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(path.display().to_string()),
            key_path: "model_providers.linear".to_string(),
            value: overlay,
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("replace succeeds");

    let replaced = crate::user_settings::snapshot(tmp.path()).await?.settings;
    let expected_replace: TomlValue = toml::from_str(
        r#"[model_providers.linear]
env_key = "NEW_TOKEN"
name = "linear"
base_url = "https://linear.example"

[model_providers.linear.query_params]
alpha = "updated"
beta = "b"
"#,
    )?;
    assert_eq!(replaced, expected_replace);

    Ok(())
}
