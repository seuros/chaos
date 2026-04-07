use super::*;
use anyhow::Result;
use chaos_ipc::api::AppConfig;
use chaos_ipc::api::AppToolApproval;
use chaos_ipc::api::AppsConfig;
use chaos_realpath::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

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
async fn write_value_preserves_comments_and_order() -> Result<()> {
    let tmp = tempdir().expect("tempdir");
    let original = r#"# Codex user configuration
model = "gpt-5"
approval_policy = "interactive"

[notice]
# Preserve this comment
hide_full_access_warning = true

[features]
unified_exec = true
"#;
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), original)?;

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "features.personality".to_string(),
            value: serde_json::json!(true),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("write succeeds");

    let updated = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    let expected = r#"# Codex user configuration
model = "gpt-5"
approval_policy = "interactive"

[notice]
# Preserve this comment
hide_full_access_warning = true

[features]
unified_exec = true
personality = true
"#;
    assert_eq!(updated, expected);
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
    let user_file = AbsolutePathBuf::try_from(user_path.clone()).expect("user file");

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
        ConfigLayerSource::User {
            file: user_file.clone()
        },
    );
    let layers = response.layers.expect("layers present");
    assert_eq!(layers.len(), 2, "expected two layers");
    assert_eq!(
        layers.first().unwrap().name,
        ConfigLayerSource::User {
            file: user_file.clone()
        }
    );
    assert!(matches!(
        layers.get(1).unwrap().name,
        ConfigLayerSource::System { .. }
    ));
}

#[tokio::test]
async fn version_conflict_rejected() {
    let tmp = tempdir().expect("tempdir");
    let user_path = tmp.path().join(CONFIG_TOML_FILE);
    std::fs::write(&user_path, "model = \"user\"").unwrap();

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    let error = service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "model".to_string(),
            value: serde_json::json!("gpt-5"),
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
async fn write_value_defaults_to_user_config_path() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), "").unwrap();

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    service
        .write_value(ConfigValueWriteParams {
            file_path: None,
            key_path: "model".to_string(),
            value: serde_json::json!("gpt-new"),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("write succeeds");

    let contents = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    assert!(
        contents.contains("model = \"gpt-new\""),
        "config.toml should be updated even when file_path is omitted"
    );
}

#[tokio::test]
async fn invalid_user_value_rejected() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), "model = \"user\"").unwrap();

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

    let contents = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    assert_eq!(contents.trim(), "model = \"user\"");
}

#[tokio::test]
async fn reserved_builtin_provider_override_rejected() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(CONFIG_TOML_FILE), "model = \"user\"\n").unwrap();

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    let error = service
        .write_value(ConfigValueWriteParams {
            file_path: Some(tmp.path().join(CONFIG_TOML_FILE).display().to_string()),
            key_path: "model_providers.openai.name".to_string(),
            value: serde_json::json!("OpenAI Override"),
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect_err("should reject reserved provider override");

    assert_eq!(
        error.write_error_code(),
        Some(ConfigWriteErrorCode::ConfigValidationError)
    );
    assert!(error.to_string().contains("reserved built-in provider IDs"));
    assert!(error.to_string().contains("`openai`"));

    let contents = std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
    assert_eq!(contents, "model = \"user\"\n");
}

#[tokio::test]
async fn read_reports_session_flags_override_user() {
    let tmp = tempdir().expect("tempdir");
    let user_path = tmp.path().join(CONFIG_TOML_FILE);
    std::fs::write(&user_path, "model = \"user\"").unwrap();
    let user_file = AbsolutePathBuf::try_from(user_path.clone()).expect("user file");

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
        ConfigLayerSource::User { file: user_file }
    );
}

#[tokio::test]
async fn upsert_merges_tables_replace_overwrites() -> Result<()> {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join(CONFIG_TOML_FILE);
    let base = r#"[mcp_servers.linear]
bearer_token_env_var = "TOKEN"
name = "linear"
url = "https://linear.example"

[mcp_servers.linear.env_http_headers]
existing = "keep"

[mcp_servers.linear.http_headers]
alpha = "a"
"#;

    let overlay = serde_json::json!({
        "bearer_token_env_var": "NEW_TOKEN",
        "http_headers": {
            "alpha": "updated",
            "beta": "b"
        },
        "name": "linear",
        "url": "https://linear.example"
    });

    std::fs::write(&path, base)?;

    let service = ConfigService::new_with_defaults(tmp.path().to_path_buf());
    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(path.display().to_string()),
            key_path: "mcp_servers.linear".to_string(),
            value: overlay.clone(),
            merge_strategy: MergeStrategy::Upsert,
            expected_version: None,
        })
        .await
        .expect("upsert succeeds");

    let upserted: TomlValue = toml::from_str(&std::fs::read_to_string(&path)?)?;
    let expected_upsert: TomlValue = toml::from_str(
        r#"[mcp_servers.linear]
bearer_token_env_var = "NEW_TOKEN"
name = "linear"
url = "https://linear.example"

[mcp_servers.linear.env_http_headers]
existing = "keep"

[mcp_servers.linear.http_headers]
alpha = "updated"
beta = "b"
"#,
    )?;
    assert_eq!(upserted, expected_upsert);

    std::fs::write(&path, base)?;

    service
        .write_value(ConfigValueWriteParams {
            file_path: Some(path.display().to_string()),
            key_path: "mcp_servers.linear".to_string(),
            value: overlay,
            merge_strategy: MergeStrategy::Replace,
            expected_version: None,
        })
        .await
        .expect("replace succeeds");

    let replaced: TomlValue = toml::from_str(&std::fs::read_to_string(&path)?)?;
    let expected_replace: TomlValue = toml::from_str(
        r#"[mcp_servers.linear]
bearer_token_env_var = "NEW_TOKEN"
name = "linear"
url = "https://linear.example"

[mcp_servers.linear.http_headers]
alpha = "updated"
beta = "b"
"#,
    )?;
    assert_eq!(replaced, expected_replace);

    Ok(())
}
