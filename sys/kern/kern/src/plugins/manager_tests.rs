use super::*;
use crate::config::CONFIG_TOML_FILE;
use crate::config::ConfigBuilder;
use crate::config::types::McpServerTransportConfig;
use crate::config_loader::ConfigLayerEntry;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::ConfigRequirements;
use crate::config_loader::ConfigRequirementsToml;
use chaos_ipc::api::ConfigLayerSource;
use pretty_assertions::assert_eq;
use std::fs;
use tempfile::TempDir;
use toml::Value;

fn write_file(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("file should have a parent")).unwrap();
    fs::write(path, contents).unwrap();
}

fn write_plugin(root: &Path, dir_name: &str, manifest_name: &str) {
    let plugin_root = root.join(dir_name);
    fs::create_dir_all(plugin_root.join(".codex-plugin")).unwrap();
    fs::create_dir_all(plugin_root.join("skills")).unwrap();
    fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{manifest_name}"}}"#),
    )
    .unwrap();
    fs::write(plugin_root.join("skills/SKILL.md"), "skill").unwrap();
    fs::write(plugin_root.join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
}

fn plugin_config_toml(enabled: bool, plugins_feature_enabled: bool) -> String {
    let mut root = toml::map::Map::new();

    let mut features = toml::map::Map::new();
    features.insert(
        "plugins".to_string(),
        Value::Boolean(plugins_feature_enabled),
    );
    root.insert("features".to_string(), Value::Table(features));

    let mut plugin = toml::map::Map::new();
    plugin.insert("enabled".to_string(), Value::Boolean(enabled));

    let mut plugins = toml::map::Map::new();
    plugins.insert("sample@test".to_string(), Value::Table(plugin));
    root.insert("plugins".to_string(), Value::Table(plugins));

    toml::to_string(&Value::Table(root)).expect("plugin test config should serialize")
}

fn load_plugins_from_config(config_toml: &str, codex_home: &Path) -> PluginLoadOutcome {
    write_file(&codex_home.join(CONFIG_TOML_FILE), config_toml);
    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: AbsolutePathBuf::try_from(codex_home.join(CONFIG_TOML_FILE)).unwrap(),
            },
            toml::from_str(config_toml).expect("plugin test config should parse"),
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack should build");
    PluginsManager::new(codex_home.to_path_buf()).plugins_for_layer_stack(codex_home, &stack, false)
}

async fn load_config(codex_home: &Path, cwd: &Path) -> crate::config::Config {
    ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .fallback_cwd(Some(cwd.to_path_buf()))
        .build()
        .await
        .expect("config should load")
}

#[test]
fn load_plugins_loads_default_skills_and_mcp_servers() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{
  "name": "sample",
  "description": "Plugin that includes the sample MCP server and Skills"
}"#,
    );
    write_file(
        &plugin_root.join("skills/sample-search/SKILL.md"),
        "---\nname: sample-search\ndescription: search sample data\n---\n",
    );
    write_file(
        &plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "sample": {
      "type": "http",
      "url": "https://sample.example/mcp",
      "oauth": {
        "clientId": "client-id",
        "callbackPort": 3118
      }
    }
  }
}"#,
    );
    write_file(
        &plugin_root.join(".app.json"),
        r#"{
  "apps": {
    "example": {
      "id": "connector_example"
    }
  }
}"#,
    );

    let outcome = load_plugins_from_config(&plugin_config_toml(true, true), codex_home.path());

    assert_eq!(
        outcome.plugins,
        vec![LoadedPlugin {
            config_name: "sample@test".to_string(),
            manifest_name: Some("sample".to_string()),
            manifest_description: Some(
                "Plugin that includes the sample MCP server and Skills".to_string(),
            ),
            root: AbsolutePathBuf::try_from(plugin_root.clone()).unwrap(),
            enabled: true,
            skill_roots: vec![plugin_root.join("skills")],
            mcp_servers: HashMap::from([(
                "sample".to_string(),
                McpServerConfig {
                    transport: McpServerTransportConfig::StreamableHttp {
                        url: "https://sample.example/mcp".to_string(),
                        bearer_token_env_var: None,
                        http_headers: None,
                        env_http_headers: None,
                    },
                    enabled: true,
                    required: false,
                    disabled_reason: None,
                    startup_timeout_sec: None,
                    tool_timeout_sec: None,
                    enabled_tools: None,
                    disabled_tools: None,
                    scopes: None,
                    oauth_resource: None,
                },
            )]),
            apps: vec![AppConnectorId("connector_example".to_string())],
            error: None,
        }]
    );
    assert_eq!(
        outcome.capability_summaries(),
        &[PluginCapabilitySummary {
            config_name: "sample@test".to_string(),
            display_name: "sample".to_string(),
            description: Some("Plugin that includes the sample MCP server and Skills".to_string(),),
            has_skills: true,
            mcp_server_names: vec!["sample".to_string()],
            app_connector_ids: vec![AppConnectorId("connector_example".to_string())],
        }]
    );
    assert_eq!(
        outcome.effective_skill_roots(),
        vec![plugin_root.join("skills")]
    );
    assert_eq!(outcome.effective_mcp_servers().len(), 1);
    assert_eq!(
        outcome.effective_apps(),
        vec![AppConnectorId("connector_example".to_string())]
    );
}

#[test]
fn plugin_telemetry_metadata_uses_default_mcp_config_path() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{
  "name": "sample"
}"#,
    );
    write_file(
        &plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "sample": {
      "type": "http",
      "url": "https://sample.example/mcp"
    }
  }
}"#,
    );

    let metadata = plugin_telemetry_metadata_from_root(
        &PluginId::parse("sample@test").expect("plugin id should parse"),
        &plugin_root,
    );

    assert_eq!(
        metadata.capability_summary,
        Some(PluginCapabilitySummary {
            config_name: "sample@test".to_string(),
            display_name: "sample".to_string(),
            description: None,
            has_skills: false,
            mcp_server_names: vec!["sample".to_string()],
            app_connector_ids: Vec::new(),
        })
    );
}

#[test]
fn capability_summary_sanitizes_plugin_descriptions_to_one_line() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{
  "name": "sample",
  "description": "Plugin that\n includes   the sample\tserver"
}"#,
    );
    write_file(
        &plugin_root.join("skills/sample-search/SKILL.md"),
        "---\nname: sample-search\ndescription: search sample data\n---\n",
    );

    let outcome = load_plugins_from_config(&plugin_config_toml(true, true), codex_home.path());

    assert_eq!(
        outcome.plugins[0].manifest_description.as_deref(),
        Some("Plugin that\n includes   the sample\tserver")
    );
    assert_eq!(
        outcome.capability_summaries()[0].description.as_deref(),
        Some("Plugin that includes the sample server")
    );
}

#[test]
fn capability_summary_truncates_overlong_plugin_descriptions() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");
    let too_long = "x".repeat(MAX_CAPABILITY_SUMMARY_DESCRIPTION_LEN + 1);

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        &format!(
            r#"{{
  "name": "sample",
  "description": "{too_long}"
}}"#
        ),
    );
    write_file(
        &plugin_root.join("skills/sample-search/SKILL.md"),
        "---\nname: sample-search\ndescription: search sample data\n---\n",
    );

    let outcome = load_plugins_from_config(&plugin_config_toml(true, true), codex_home.path());

    assert_eq!(
        outcome.plugins[0].manifest_description.as_deref(),
        Some(too_long.as_str())
    );
    assert_eq!(
        outcome.capability_summaries()[0].description,
        Some("x".repeat(MAX_CAPABILITY_SUMMARY_DESCRIPTION_LEN))
    );
}

#[test]
fn load_plugins_uses_manifest_configured_component_paths() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{
  "name": "sample",
  "skills": "./custom-skills/",
  "mcpServers": "./config/custom.mcp.json",
  "apps": "./config/custom.app.json"
}"#,
    );
    write_file(
        &plugin_root.join("skills/default-skill/SKILL.md"),
        "---\nname: default-skill\ndescription: default skill\n---\n",
    );
    write_file(
        &plugin_root.join("custom-skills/custom-skill/SKILL.md"),
        "---\nname: custom-skill\ndescription: custom skill\n---\n",
    );
    write_file(
        &plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "default": {
      "type": "http",
      "url": "https://default.example/mcp"
    }
  }
}"#,
    );
    write_file(
        &plugin_root.join("config/custom.mcp.json"),
        r#"{
  "mcpServers": {
    "custom": {
      "type": "http",
      "url": "https://custom.example/mcp"
    }
  }
}"#,
    );
    write_file(
        &plugin_root.join(".app.json"),
        r#"{
  "apps": {
    "default": {
      "id": "connector_default"
    }
  }
}"#,
    );
    write_file(
        &plugin_root.join("config/custom.app.json"),
        r#"{
  "apps": {
    "custom": {
      "id": "connector_custom"
    }
  }
}"#,
    );

    let outcome = load_plugins_from_config(&plugin_config_toml(true, true), codex_home.path());

    assert_eq!(
        outcome.plugins[0].skill_roots,
        vec![
            plugin_root.join("custom-skills"),
            plugin_root.join("skills")
        ]
    );
    assert_eq!(
        outcome.plugins[0].mcp_servers,
        HashMap::from([(
            "custom".to_string(),
            McpServerConfig {
                transport: McpServerTransportConfig::StreamableHttp {
                    url: "https://custom.example/mcp".to_string(),
                    bearer_token_env_var: None,
                    http_headers: None,
                    env_http_headers: None,
                },
                enabled: true,
                required: false,
                disabled_reason: None,
                startup_timeout_sec: None,
                tool_timeout_sec: None,
                enabled_tools: None,
                disabled_tools: None,
                scopes: None,
                oauth_resource: None,
            },
        )])
    );
    assert_eq!(
        outcome.plugins[0].apps,
        vec![AppConnectorId("connector_custom".to_string())]
    );
}

#[test]
fn load_plugins_ignores_manifest_component_paths_without_dot_slash() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{
  "name": "sample",
  "skills": "custom-skills",
  "mcpServers": "config/custom.mcp.json",
  "apps": "config/custom.app.json"
}"#,
    );
    write_file(
        &plugin_root.join("skills/default-skill/SKILL.md"),
        "---\nname: default-skill\ndescription: default skill\n---\n",
    );
    write_file(
        &plugin_root.join("custom-skills/custom-skill/SKILL.md"),
        "---\nname: custom-skill\ndescription: custom skill\n---\n",
    );
    write_file(
        &plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "default": {
      "type": "http",
      "url": "https://default.example/mcp"
    }
  }
}"#,
    );
    write_file(
        &plugin_root.join("config/custom.mcp.json"),
        r#"{
  "mcpServers": {
    "custom": {
      "type": "http",
      "url": "https://custom.example/mcp"
    }
  }
}"#,
    );
    write_file(
        &plugin_root.join(".app.json"),
        r#"{
  "apps": {
    "default": {
      "id": "connector_default"
    }
  }
}"#,
    );
    write_file(
        &plugin_root.join("config/custom.app.json"),
        r#"{
  "apps": {
    "custom": {
      "id": "connector_custom"
    }
  }
}"#,
    );

    let outcome = load_plugins_from_config(&plugin_config_toml(true, true), codex_home.path());

    assert_eq!(
        outcome.plugins[0].skill_roots,
        vec![plugin_root.join("skills")]
    );
    assert_eq!(
        outcome.plugins[0].mcp_servers,
        HashMap::from([(
            "default".to_string(),
            McpServerConfig {
                transport: McpServerTransportConfig::StreamableHttp {
                    url: "https://default.example/mcp".to_string(),
                    bearer_token_env_var: None,
                    http_headers: None,
                    env_http_headers: None,
                },
                enabled: true,
                required: false,
                disabled_reason: None,
                startup_timeout_sec: None,
                tool_timeout_sec: None,
                enabled_tools: None,
                disabled_tools: None,
                scopes: None,
                oauth_resource: None,
            },
        )])
    );
    assert_eq!(
        outcome.plugins[0].apps,
        vec![AppConnectorId("connector_default".to_string())]
    );
}

#[test]
fn load_plugins_preserves_disabled_plugins_without_effective_contributions() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"sample"}"#,
    );
    write_file(
        &plugin_root.join(".mcp.json"),
        r#"{
  "mcpServers": {
    "sample": {
      "type": "http",
      "url": "https://sample.example/mcp"
    }
  }
}"#,
    );

    let outcome = load_plugins_from_config(&plugin_config_toml(false, true), codex_home.path());

    assert_eq!(
        outcome.plugins,
        vec![LoadedPlugin {
            config_name: "sample@test".to_string(),
            manifest_name: None,
            manifest_description: None,
            root: AbsolutePathBuf::try_from(plugin_root).unwrap(),
            enabled: false,
            skill_roots: Vec::new(),
            mcp_servers: HashMap::new(),
            apps: Vec::new(),
            error: None,
        }]
    );
    assert!(outcome.effective_skill_roots().is_empty());
    assert!(outcome.effective_mcp_servers().is_empty());
}

#[test]
fn effective_apps_dedupes_connector_ids_across_plugins() {
    let codex_home = TempDir::new().unwrap();
    let plugin_a_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/plugin-a/local");
    let plugin_b_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/plugin-b/local");

    write_file(
        &plugin_a_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"plugin-a"}"#,
    );
    write_file(
        &plugin_a_root.join(".app.json"),
        r#"{
  "apps": {
    "example": {
      "id": "connector_example"
    }
  }
}"#,
    );
    write_file(
        &plugin_b_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"plugin-b"}"#,
    );
    write_file(
        &plugin_b_root.join(".app.json"),
        r#"{
  "apps": {
    "chat": {
      "id": "connector_example"
    },
    "gmail": {
      "id": "connector_gmail"
    }
  }
}"#,
    );

    let mut root = toml::map::Map::new();
    let mut features = toml::map::Map::new();
    features.insert("plugins".to_string(), Value::Boolean(true));
    root.insert("features".to_string(), Value::Table(features));

    let mut plugins = toml::map::Map::new();

    let mut plugin_a = toml::map::Map::new();
    plugin_a.insert("enabled".to_string(), Value::Boolean(true));
    plugins.insert("plugin-a@test".to_string(), Value::Table(plugin_a));

    let mut plugin_b = toml::map::Map::new();
    plugin_b.insert("enabled".to_string(), Value::Boolean(true));
    plugins.insert("plugin-b@test".to_string(), Value::Table(plugin_b));

    root.insert("plugins".to_string(), Value::Table(plugins));
    let config_toml =
        toml::to_string(&Value::Table(root)).expect("plugin test config should serialize");

    let outcome = load_plugins_from_config(&config_toml, codex_home.path());

    assert_eq!(
        outcome.effective_apps(),
        vec![
            AppConnectorId("connector_example".to_string()),
            AppConnectorId("connector_gmail".to_string()),
        ]
    );
}

#[test]
fn capability_index_filters_inactive_and_zero_capability_plugins() {
    let codex_home = TempDir::new().unwrap();
    let connector = |id: &str| AppConnectorId(id.to_string());
    let http_server = |url: &str| McpServerConfig {
        transport: McpServerTransportConfig::StreamableHttp {
            url: url.to_string(),
            bearer_token_env_var: None,
            http_headers: None,
            env_http_headers: None,
        },
        enabled: true,
        required: false,
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth_resource: None,
    };
    let plugin = |config_name: &str, dir_name: &str, manifest_name: &str| LoadedPlugin {
        config_name: config_name.to_string(),
        manifest_name: Some(manifest_name.to_string()),
        manifest_description: None,
        root: AbsolutePathBuf::try_from(codex_home.path().join(dir_name)).unwrap(),
        enabled: true,
        skill_roots: Vec::new(),
        mcp_servers: HashMap::new(),
        apps: Vec::new(),
        error: None,
    };
    let summary = |config_name: &str, display_name: &str| PluginCapabilitySummary {
        config_name: config_name.to_string(),
        display_name: display_name.to_string(),
        description: None,
        ..PluginCapabilitySummary::default()
    };
    let outcome = PluginLoadOutcome::from_plugins(vec![
        LoadedPlugin {
            skill_roots: vec![codex_home.path().join("skills-plugin/skills")],
            ..plugin("skills@test", "skills-plugin", "skills-plugin")
        },
        LoadedPlugin {
            mcp_servers: HashMap::from([("alpha".to_string(), http_server("https://alpha"))]),
            apps: vec![connector("connector_example")],
            ..plugin("alpha@test", "alpha-plugin", "alpha-plugin")
        },
        LoadedPlugin {
            mcp_servers: HashMap::from([("beta".to_string(), http_server("https://beta"))]),
            apps: vec![connector("connector_example"), connector("connector_gmail")],
            ..plugin("beta@test", "beta-plugin", "beta-plugin")
        },
        plugin("empty@test", "empty-plugin", "empty-plugin"),
        LoadedPlugin {
            enabled: false,
            skill_roots: vec![codex_home.path().join("disabled-plugin/skills")],
            apps: vec![connector("connector_hidden")],
            ..plugin("disabled@test", "disabled-plugin", "disabled-plugin")
        },
        LoadedPlugin {
            apps: vec![connector("connector_broken")],
            error: Some("failed to load".to_string()),
            ..plugin("broken@test", "broken-plugin", "broken-plugin")
        },
    ]);

    assert_eq!(
        outcome.capability_summaries(),
        &[
            PluginCapabilitySummary {
                has_skills: true,
                ..summary("skills@test", "skills-plugin")
            },
            PluginCapabilitySummary {
                mcp_server_names: vec!["alpha".to_string()],
                app_connector_ids: vec![connector("connector_example")],
                ..summary("alpha@test", "alpha-plugin")
            },
            PluginCapabilitySummary {
                mcp_server_names: vec!["beta".to_string()],
                app_connector_ids: vec![
                    connector("connector_example"),
                    connector("connector_gmail"),
                ],
                ..summary("beta@test", "beta-plugin")
            },
        ]
    );
}

#[test]
fn load_plugins_returns_empty_when_feature_disabled() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"sample"}"#,
    );
    write_file(
        &plugin_root.join("skills/sample-search/SKILL.md"),
        "---\nname: sample-search\ndescription: search sample data\n---\n",
    );

    let outcome = load_plugins_from_config(&plugin_config_toml(true, false), codex_home.path());

    assert_eq!(outcome, PluginLoadOutcome::default());
}

#[test]
fn load_plugins_rejects_invalid_plugin_keys() {
    let codex_home = TempDir::new().unwrap();
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"sample"}"#,
    );

    let mut root = toml::map::Map::new();
    let mut features = toml::map::Map::new();
    features.insert("plugins".to_string(), Value::Boolean(true));
    root.insert("features".to_string(), Value::Table(features));

    let mut plugin = toml::map::Map::new();
    plugin.insert("enabled".to_string(), Value::Boolean(true));

    let mut plugins = toml::map::Map::new();
    plugins.insert("sample".to_string(), Value::Table(plugin));
    root.insert("plugins".to_string(), Value::Table(plugins));

    let outcome = load_plugins_from_config(
        &toml::to_string(&Value::Table(root)).expect("plugin test config should serialize"),
        codex_home.path(),
    );

    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(
        outcome.plugins[0].error.as_deref(),
        Some("invalid plugin key `sample`; expected <plugin>@<marketplace>")
    );
    assert!(outcome.effective_skill_roots().is_empty());
    assert!(outcome.effective_mcp_servers().is_empty());
}

#[tokio::test]
async fn install_plugin_updates_config_with_relative_path_and_plugin_key() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).unwrap();
    fs::create_dir_all(repo_root.join(".agents/plugins")).unwrap();
    write_plugin(&repo_root, "sample-plugin", "sample-plugin");
    fs::write(
        repo_root.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "sample-plugin",
      "source": {
        "source": "local",
        "path": "./sample-plugin"
      },
      "authPolicy": "ON_USE"
    }
  ]
}"#,
    )
    .unwrap();

    let result = PluginsManager::new(tmp.path().to_path_buf())
        .install_plugin(PluginInstallRequest {
            plugin_name: "sample-plugin".to_string(),
            marketplace_path: AbsolutePathBuf::try_from(
                repo_root.join(".agents/plugins/marketplace.json"),
            )
            .unwrap(),
        })
        .await
        .unwrap();

    let installed_path = tmp.path().join("plugins/cache/debug/sample-plugin/local");
    assert_eq!(
        result,
        PluginInstallOutcome {
            plugin_id: PluginId::new("sample-plugin".to_string(), "debug".to_string()).unwrap(),
            plugin_version: "local".to_string(),
            installed_path: AbsolutePathBuf::try_from(installed_path).unwrap(),
            auth_policy: MarketplacePluginAuthPolicy::OnUse,
        }
    );

    let config = fs::read_to_string(tmp.path().join("config.toml")).unwrap();
    assert!(config.contains(r#"[plugins."sample-plugin@debug"]"#));
    assert!(config.contains("enabled = true"));
}

#[tokio::test]
async fn uninstall_plugin_removes_cache_and_config_entry() {
    let tmp = tempfile::tempdir().unwrap();
    write_plugin(
        &tmp.path().join("plugins/cache/debug"),
        "sample-plugin/local",
        "sample-plugin",
    );
    write_file(
        &tmp.path().join(CONFIG_TOML_FILE),
        r#"[features]
plugins = true

[plugins."sample-plugin@debug"]
enabled = true
"#,
    );

    let manager = PluginsManager::new(tmp.path().to_path_buf());
    manager
        .uninstall_plugin("sample-plugin@debug".to_string())
        .await
        .unwrap();
    manager
        .uninstall_plugin("sample-plugin@debug".to_string())
        .await
        .unwrap();

    assert!(
        !tmp.path()
            .join("plugins/cache/debug/sample-plugin")
            .exists()
    );
    let config = fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).unwrap();
    assert!(!config.contains(r#"[plugins."sample-plugin@debug"]"#));
}

#[tokio::test]
async fn list_marketplaces_includes_enabled_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).unwrap();
    fs::create_dir_all(repo_root.join(".agents/plugins")).unwrap();
    write_plugin(
        &tmp.path().join("plugins/cache/debug"),
        "enabled-plugin/local",
        "enabled-plugin",
    );
    write_plugin(
        &tmp.path().join("plugins/cache/debug"),
        "disabled-plugin/local",
        "disabled-plugin",
    );
    fs::write(
        repo_root.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "enabled-plugin",
      "source": {
        "source": "local",
        "path": "./enabled-plugin"
      }
    },
    {
      "name": "disabled-plugin",
      "source": {
        "source": "local",
        "path": "./disabled-plugin"
      }
    }
  ]
}"#,
    )
    .unwrap();
    write_file(
        &tmp.path().join(CONFIG_TOML_FILE),
        r#"[features]
plugins = true

[plugins."enabled-plugin@debug"]
enabled = true

[plugins."disabled-plugin@debug"]
enabled = false
"#,
    );

    let config = load_config(tmp.path(), &repo_root).await;
    let marketplaces = PluginsManager::new(tmp.path().to_path_buf())
        .list_marketplaces_for_config(&config, &[AbsolutePathBuf::try_from(repo_root).unwrap()])
        .unwrap();

    let marketplace = marketplaces
        .into_iter()
        .find(|marketplace| {
            marketplace.path
                == AbsolutePathBuf::try_from(
                    tmp.path().join("repo/.agents/plugins/marketplace.json"),
                )
                .unwrap()
        })
        .expect("expected repo marketplace entry");

    assert_eq!(
        marketplace,
        ConfiguredMarketplaceSummary {
            name: "debug".to_string(),
            path: AbsolutePathBuf::try_from(
                tmp.path().join("repo/.agents/plugins/marketplace.json"),
            )
            .unwrap(),
            display_name: None,
            plugins: vec![
                ConfiguredMarketplacePluginSummary {
                    id: "enabled-plugin@debug".to_string(),
                    name: "enabled-plugin".to_string(),
                    source: MarketplacePluginSourceSummary::Local {
                        path: AbsolutePathBuf::try_from(tmp.path().join("repo/enabled-plugin"))
                            .unwrap(),
                    },
                    install_policy: MarketplacePluginInstallPolicy::Available,
                    auth_policy: MarketplacePluginAuthPolicy::OnInstall,
                    interface: None,
                    installed: true,
                    enabled: true,
                },
                ConfiguredMarketplacePluginSummary {
                    id: "disabled-plugin@debug".to_string(),
                    name: "disabled-plugin".to_string(),
                    source: MarketplacePluginSourceSummary::Local {
                        path: AbsolutePathBuf::try_from(tmp.path().join("repo/disabled-plugin"),)
                            .unwrap(),
                    },
                    install_policy: MarketplacePluginInstallPolicy::Available,
                    auth_policy: MarketplacePluginAuthPolicy::OnInstall,
                    interface: None,
                    installed: true,
                    enabled: false,
                },
            ],
        }
    );
}

#[tokio::test]
async fn list_marketplaces_uses_first_duplicate_plugin_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_a_root = tmp.path().join("repo-a");
    let repo_b_root = tmp.path().join("repo-b");
    fs::create_dir_all(repo_a_root.join(".git")).unwrap();
    fs::create_dir_all(repo_b_root.join(".git")).unwrap();
    fs::create_dir_all(repo_a_root.join(".agents/plugins")).unwrap();
    fs::create_dir_all(repo_b_root.join(".agents/plugins")).unwrap();
    fs::write(
        repo_a_root.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "dup-plugin",
      "source": {
        "source": "local",
        "path": "./from-a"
      }
    }
  ]
}"#,
    )
    .unwrap();
    fs::write(
        repo_b_root.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "dup-plugin",
      "source": {
        "source": "local",
        "path": "./from-b"
      }
    },
    {
      "name": "b-only-plugin",
      "source": {
        "source": "local",
        "path": "./from-b-only"
      }
    }
  ]
}"#,
    )
    .unwrap();
    write_file(
        &tmp.path().join(CONFIG_TOML_FILE),
        r#"[features]
plugins = true

[plugins."dup-plugin@debug"]
enabled = true

[plugins."b-only-plugin@debug"]
enabled = false
"#,
    );

    let config = load_config(tmp.path(), &repo_a_root).await;
    let marketplaces = PluginsManager::new(tmp.path().to_path_buf())
        .list_marketplaces_for_config(
            &config,
            &[
                AbsolutePathBuf::try_from(repo_a_root).unwrap(),
                AbsolutePathBuf::try_from(repo_b_root).unwrap(),
            ],
        )
        .unwrap();

    let repo_a_marketplace = marketplaces
        .iter()
        .find(|marketplace| {
            marketplace.path
                == AbsolutePathBuf::try_from(
                    tmp.path().join("repo-a/.agents/plugins/marketplace.json"),
                )
                .unwrap()
        })
        .expect("repo-a marketplace should be listed");
    assert_eq!(
        repo_a_marketplace.plugins,
        vec![ConfiguredMarketplacePluginSummary {
            id: "dup-plugin@debug".to_string(),
            name: "dup-plugin".to_string(),
            source: MarketplacePluginSourceSummary::Local {
                path: AbsolutePathBuf::try_from(tmp.path().join("repo-a/from-a")).unwrap(),
            },
            install_policy: MarketplacePluginInstallPolicy::Available,
            auth_policy: MarketplacePluginAuthPolicy::OnInstall,
            interface: None,
            installed: false,
            enabled: true,
        }]
    );

    let repo_b_marketplace = marketplaces
        .iter()
        .find(|marketplace| {
            marketplace.path
                == AbsolutePathBuf::try_from(
                    tmp.path().join("repo-b/.agents/plugins/marketplace.json"),
                )
                .unwrap()
        })
        .expect("repo-b marketplace should be listed");
    assert_eq!(
        repo_b_marketplace.plugins,
        vec![ConfiguredMarketplacePluginSummary {
            id: "b-only-plugin@debug".to_string(),
            name: "b-only-plugin".to_string(),
            source: MarketplacePluginSourceSummary::Local {
                path: AbsolutePathBuf::try_from(tmp.path().join("repo-b/from-b-only")).unwrap(),
            },
            install_policy: MarketplacePluginInstallPolicy::Available,
            auth_policy: MarketplacePluginAuthPolicy::OnInstall,
            interface: None,
            installed: false,
            enabled: false,
        }]
    );

    let duplicate_plugin_count = marketplaces
        .iter()
        .flat_map(|marketplace| marketplace.plugins.iter())
        .filter(|plugin| plugin.name == "dup-plugin")
        .count();
    assert_eq!(duplicate_plugin_count, 1);
}

#[tokio::test]
async fn list_marketplaces_marks_configured_plugin_uninstalled_when_cache_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    fs::create_dir_all(repo_root.join(".git")).unwrap();
    fs::create_dir_all(repo_root.join(".agents/plugins")).unwrap();
    fs::write(
        repo_root.join(".agents/plugins/marketplace.json"),
        r#"{
  "name": "debug",
  "plugins": [
    {
      "name": "sample-plugin",
      "source": {
        "source": "local",
        "path": "./sample-plugin"
      }
    }
  ]
}"#,
    )
    .unwrap();
    write_file(
        &tmp.path().join(CONFIG_TOML_FILE),
        r#"[features]
plugins = true

[plugins."sample-plugin@debug"]
enabled = true
"#,
    );

    let config = load_config(tmp.path(), &repo_root).await;
    let marketplaces = PluginsManager::new(tmp.path().to_path_buf())
        .list_marketplaces_for_config(&config, &[AbsolutePathBuf::try_from(repo_root).unwrap()])
        .unwrap();

    let marketplace = marketplaces
        .into_iter()
        .find(|marketplace| {
            marketplace.path
                == AbsolutePathBuf::try_from(
                    tmp.path().join("repo/.agents/plugins/marketplace.json"),
                )
                .unwrap()
        })
        .expect("expected repo marketplace entry");

    assert_eq!(
        marketplace,
        ConfiguredMarketplaceSummary {
            name: "debug".to_string(),
            path: AbsolutePathBuf::try_from(
                tmp.path().join("repo/.agents/plugins/marketplace.json"),
            )
            .unwrap(),
            display_name: None,
            plugins: vec![ConfiguredMarketplacePluginSummary {
                id: "sample-plugin@debug".to_string(),
                name: "sample-plugin".to_string(),
                source: MarketplacePluginSourceSummary::Local {
                    path: AbsolutePathBuf::try_from(tmp.path().join("repo/sample-plugin")).unwrap(),
                },
                install_policy: MarketplacePluginInstallPolicy::Available,
                auth_policy: MarketplacePluginAuthPolicy::OnInstall,
                interface: None,
                installed: false,
                enabled: true,
            }],
        }
    );
}

#[test]
fn load_plugins_ignores_project_config_files() {
    let codex_home = TempDir::new().unwrap();
    let project_root = codex_home.path().join("project");
    let plugin_root = codex_home
        .path()
        .join("plugins/cache")
        .join("test/sample/local");

    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"sample"}"#,
    );
    write_file(
        &project_root.join(".codex/config.toml"),
        &plugin_config_toml(true, true),
    );

    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::Project {
                dot_codex_folder: AbsolutePathBuf::try_from(project_root.join(".codex")).unwrap(),
            },
            toml::from_str(&plugin_config_toml(true, true)).expect("project config should parse"),
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack should build");

    let outcome = PluginsManager::new(codex_home.path().to_path_buf()).plugins_for_layer_stack(
        &project_root,
        &stack,
        false,
    );

    assert_eq!(outcome, PluginLoadOutcome::default());
}
