use super::LoaderOverrides;
use super::load_config_layers_state;
use super::resolve_relative_paths_in_config_toml;
use crate::config::ConfigBuilder;
use crate::config::ConfigOverrides;
use crate::config_loader::AssetFolder;
use crate::config_loader::AssetScope;
use crate::config_loader::ConfigLayerEntry;
use crate::config_loader::ConfigRequirements;
use crate::config_loader::ConfigRequirementsWithSources;
use crate::config_loader::load_requirements_toml;
use crate::config_loader::version_for_toml;
use crate::runtime_db;
use crate::test_support::bootstrap_sqlite;
use crate::test_support::seed_user_settings;
use crate::user_settings::BootstrapConfig;
use crate::user_settings::ResolvedBootstrap;
use chaos_ipc::config_types::TrustLevel;
use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_realpath::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use tempfile::tempdir;
use toml::Value as TomlValue;

#[test]
fn ensure_resolve_relative_paths_in_config_toml_preserves_all_fields() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let base_dir = tmp.path();
    let contents = format!(
        r#"
# This is a field recognized by config.toml that is an AbsolutePathBuf in
# the ConfigToml struct.
model_instructions_file = "./some_file.md"

# This is a field recognized by config.toml.
model = "{}"

# This is a field not recognized by config.toml.
foo = "xyzzy"
"#,
        chaos_test_fixtures::TEST_MODEL
    );
    let user_config: TomlValue = toml::from_str(&contents)?;

    let normalized_toml_value = resolve_relative_paths_in_config_toml(user_config, base_dir)?;
    let mut expected_toml_value = toml::map::Map::new();
    expected_toml_value.insert(
        "model_instructions_file".to_string(),
        TomlValue::String(
            AbsolutePathBuf::resolve_path_against_base("./some_file.md", base_dir)
                .as_path()
                .to_string_lossy()
                .to_string(),
        ),
    );
    expected_toml_value.insert(
        "model".to_string(),
        TomlValue::String(chaos_test_fixtures::TEST_MODEL.to_string()),
    );
    expected_toml_value.insert("foo".to_string(), TomlValue::String("xyzzy".to_string()));
    assert_eq!(normalized_toml_value, TomlValue::Table(expected_toml_value));
    Ok(())
}

/// Bootstrap SQLite storage in `chaos_home`, seed optional project root
/// markers as user settings, and record a trust decision for `project_path`.
async fn make_config_for_test(
    chaos_home: &Path,
    project_path: &Path,
    trust_level: TrustLevel,
    project_root_markers: Option<Vec<String>>,
) -> std::io::Result<()> {
    let mut settings = toml::map::Map::new();
    if let Some(markers) = project_root_markers {
        settings.insert(
            "project_root_markers".to_string(),
            TomlValue::Array(markers.into_iter().map(TomlValue::String).collect()),
        );
    }
    seed_user_settings(chaos_home, TomlValue::Table(settings))
        .await
        .map_err(std::io::Error::other)?;
    let bootstrap = ResolvedBootstrap::sqlite_in(chaos_home);
    let runtime =
        runtime_db::open_or_create_runtime_db(&bootstrap.storage_url, chaos_home, "test-provider")
            .await
            .map_err(std::io::Error::other)?;
    runtime
        .set_project_trust(
            runtime_db::normalize_cwd_for_runtime_db(project_path).as_path(),
            trust_level,
        )
        .await
        .map_err(std::io::Error::other)
}

async fn load_layers(
    chaos_home: &Path,
    cwd: &Path,
) -> std::io::Result<crate::config_loader::ConfigLayerStack> {
    load_config_layers_state(
        chaos_home,
        Some(AbsolutePathBuf::from_absolute_path(cwd)?),
        &[] as &[(String, TomlValue)],
        LoaderOverrides::default(),
    )
    .await
}

fn project_asset_dirs(stack: &crate::config_loader::ConfigLayerStack) -> Vec<PathBuf> {
    stack
        .asset_folders()
        .iter()
        .filter(|folder| folder.scope == AssetScope::Project)
        .map(|folder| folder.dir.to_path_buf())
        .collect()
}

#[tokio::test]
async fn cli_overrides_resolve_relative_paths_against_cwd() -> std::io::Result<()> {
    let chaos_home = tempdir().expect("tempdir");
    bootstrap_sqlite(chaos_home.path()).map_err(std::io::Error::other)?;
    let cwd_dir = tempdir().expect("tempdir");
    let cwd_path = cwd_dir.path().to_path_buf();

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .cli_overrides(vec![(
            "log_dir".to_string(),
            TomlValue::String("run-logs".to_string()),
        )])
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd_path.clone()),
            ..Default::default()
        })
        .build()
        .await?;

    let expected = AbsolutePathBuf::resolve_path_against_base("run-logs", cwd_path);
    assert_eq!(config.log_dir, expected.to_path_buf());
    Ok(())
}

#[tokio::test]
async fn returns_config_error_for_invalid_bootstrap_toml() {
    let tmp = tempdir().expect("tempdir");
    let contents = "storage_url = \"sqlite:///tmp/x.sqlite\"\ninvalid = [";
    std::fs::write(BootstrapConfig::path(tmp.path()), contents).expect("write config");

    let err = load_layers(tmp.path(), tmp.path())
        .await
        .expect_err("expected error");

    assert!(err.to_string().contains("invalid bootstrap file"), "{err}");
}

#[tokio::test]
async fn legacy_bootstrap_file_is_rejected_with_hint() {
    let tmp = tempdir().expect("tempdir");
    let contents = "model_context_window = 1000\nstorage_url = \"sqlite:///tmp/x.sqlite\"\n";
    std::fs::write(BootstrapConfig::path(tmp.path()), contents).expect("write config");

    let err = ConfigBuilder::default()
        .chaos_home(tmp.path().to_path_buf())
        .fallback_cwd(Some(tmp.path().to_path_buf()))
        .build()
        .await
        .expect_err("expected error");

    let message = err.to_string();
    assert!(
        message.contains("`model_context_window` is not a bootstrap setting"),
        "{message}"
    );
    assert!(
        message.contains("chaos config set model_context_window"),
        "{message}"
    );
}

#[tokio::test]
async fn sqlite_home_key_is_rejected_with_hint() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(
        BootstrapConfig::path(tmp.path()),
        format!("sqlite_home = {:?}\n", tmp.path()),
    )
    .expect("write config");

    let err = load_layers(tmp.path(), tmp.path())
        .await
        .expect_err("expected error");

    let message = err.to_string();
    assert!(message.contains("`sqlite_home` was removed"), "{message}");
    assert!(message.contains("bootstrap set storage_url"), "{message}");
}

#[tokio::test]
async fn rejects_legacy_global_mcp_servers_in_cli_overrides() {
    let tmp = tempdir().expect("tempdir");
    bootstrap_sqlite(tmp.path()).expect("bootstrap");
    let cwd = AbsolutePathBuf::try_from(tmp.path()).expect("cwd");
    let err = load_config_layers_state(
        tmp.path(),
        Some(cwd),
        &[(
            "mcp_servers.docs.command".to_string(),
            TomlValue::String("echo".to_string()),
        )],
        LoaderOverrides::default(),
    )
    .await
    .expect_err("expected error");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("top-level `mcp_servers`"));
}

#[tokio::test]
async fn returns_empty_user_layer_when_nothing_is_configured() {
    let tmp = tempdir().expect("tempdir");
    bootstrap_sqlite(tmp.path()).expect("bootstrap");

    let layers = load_layers(tmp.path(), tmp.path())
        .await
        .expect("load layers");
    let user_layer = layers
        .get_user_layer()
        .expect("expected a user layer even when no settings exist");
    assert_eq!(
        &ConfigLayerEntry {
            name: super::ConfigLayerSource::UserDatabase { revision: 0 },
            config: TomlValue::Table(toml::map::Map::new()),
            version: format!(
                "db:0:{}",
                version_for_toml(&TomlValue::Table(toml::map::Map::new()))
            ),
            disabled_reason: None,
        },
        user_layer,
    );

    let effective = layers.effective_config();
    let table = effective.as_table().expect("top-level table expected");
    assert!(
        !table.contains_key("storage_url") && !table.contains_key("egress_url"),
        "bootstrap values never enter the settings document"
    );
    assert_eq!(layers.layers_high_to_low().len(), 1);
}

#[tokio::test]
async fn asset_folders_list_admin_and_trusted_projects() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    tokio::fs::create_dir_all(nested.join(".chaos")).await?;
    tokio::fs::create_dir_all(project_root.join(".chaos")).await?;
    tokio::fs::write(project_root.join(".git"), "gitdir: here").await?;

    let chaos_home = tmp.path().join("home");
    tokio::fs::create_dir_all(&chaos_home).await?;
    make_config_for_test(&chaos_home, &project_root, TrustLevel::Trusted, None).await?;
    let layers = load_layers(&chaos_home, &nested).await?;

    let folders = layers.asset_folders();
    assert_eq!(folders[0].scope, AssetScope::Admin);
    assert_eq!(folders[0].dir.as_path(), Path::new("/etc/chaos"));
    assert!(
        folders
            .iter()
            .all(|folder| folder.dir.as_path() != chaos_home.as_path()),
        "the home is not an asset folder; user rules and roles live in the database"
    );
    assert_eq!(
        project_asset_dirs(&layers),
        vec![project_root.join(".chaos"), nested.join(".chaos")],
        "project folders run from the project root toward cwd"
    );
    assert!(folders.iter().all(AssetFolder::is_enabled));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn load_requirements_toml_produces_expected_constraints() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let requirements_file = tmp.path().join("requirements.toml");
    tokio::fs::write(
        &requirements_file,
        r#"
allowed_approval_policies = ["headless", "interactive"]
allowed_web_search_modes = ["cached"]
enforce_residency = "us"
"#,
    )
    .await?;

    let mut config_requirements_toml = ConfigRequirementsWithSources::default();
    load_requirements_toml(&mut config_requirements_toml, &requirements_file).await?;

    assert_eq!(
        config_requirements_toml
            .allowed_approval_policies
            .as_deref()
            .cloned(),
        Some(vec![ApprovalPolicy::Headless, ApprovalPolicy::Interactive])
    );
    assert_eq!(
        config_requirements_toml
            .allowed_web_search_modes
            .as_deref()
            .cloned(),
        Some(vec![crate::config_loader::WebSearchModeRequirement::Cached])
    );
    let config_requirements: ConfigRequirements = config_requirements_toml.try_into()?;
    assert_eq!(
        config_requirements.approval_policy.value(),
        ApprovalPolicy::Headless
    );
    config_requirements
        .approval_policy
        .can_set(&ApprovalPolicy::Headless)?;
    assert!(
        config_requirements
            .approval_policy
            .can_set(&ApprovalPolicy::Supervised)
            .is_err()
    );
    assert_eq!(
        config_requirements.web_search_mode.value(),
        WebSearchMode::Cached
    );
    config_requirements
        .web_search_mode
        .can_set(&WebSearchMode::Cached)?;
    config_requirements
        .web_search_mode
        .can_set(&WebSearchMode::Cached)?;
    config_requirements
        .web_search_mode
        .can_set(&WebSearchMode::Disabled)?;
    assert!(
        config_requirements
            .web_search_mode
            .can_set(&WebSearchMode::Live)
            .is_err()
    );
    assert_eq!(
        config_requirements.enforce_residency.value(),
        Some(crate::config_loader::ResidencyRequirement::Us)
    );
    Ok(())
}

#[tokio::test]
async fn cli_override_model_instructions_file_sets_base_instructions() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let chaos_home = tmp.path().join("home");
    tokio::fs::create_dir_all(&chaos_home).await?;
    bootstrap_sqlite(&chaos_home).map_err(std::io::Error::other)?;

    let cwd = tmp.path().join("work");
    tokio::fs::create_dir_all(&cwd).await?;

    let instructions_path = tmp.path().join("instr.md");
    tokio::fs::write(&instructions_path, "cli override instructions").await?;

    let cli_overrides = vec![(
        "model_instructions_file".to_string(),
        TomlValue::String(instructions_path.to_string_lossy().to_string()),
    )];

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home)
        .cli_overrides(cli_overrides)
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd),
            ..ConfigOverrides::default()
        })
        .build()
        .await?;

    assert_eq!(
        config.base_instructions.as_deref(),
        Some("cli override instructions")
    );

    Ok(())
}

#[tokio::test]
async fn chaos_home_is_not_listed_as_project_folder_from_home_dir() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let home_dir = tmp.path().join("home");
    let chaos_home = home_dir.join(".chaos");
    tokio::fs::create_dir_all(&chaos_home).await?;
    seed_user_settings(
        &chaos_home,
        toml::from_str("model = \"user\"\n").expect("settings"),
    )
    .await
    .map_err(std::io::Error::other)?;

    let layers = load_layers(&chaos_home, &home_dir).await?;

    assert_eq!(project_asset_dirs(&layers), Vec::<PathBuf>::new());
    assert_eq!(
        layers.effective_config().get("model"),
        Some(&TomlValue::String("user".to_string()))
    );

    Ok(())
}

#[tokio::test]
async fn chaos_home_within_project_tree_is_not_double_listed() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    let project_dot_chaos = project_root.join(".chaos");
    let nested_dot_chaos = nested.join(".chaos");

    tokio::fs::create_dir_all(&nested_dot_chaos).await?;
    tokio::fs::create_dir_all(project_root.join(".git")).await?;
    tokio::fs::create_dir_all(&project_dot_chaos).await?;
    make_config_for_test(&project_dot_chaos, &project_root, TrustLevel::Trusted, None).await?;

    let layers = load_layers(&project_dot_chaos, &nested).await?;

    assert_eq!(project_asset_dirs(&layers), vec![nested_dot_chaos]);
    assert!(
        layers
            .asset_folders()
            .iter()
            .all(|folder| folder.dir.as_path() != project_dot_chaos.as_path()),
        "a chaos home inside the project tree is not listed as a project folder"
    );

    Ok(())
}

#[tokio::test]
async fn project_folders_disabled_when_untrusted_or_unknown() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    tokio::fs::create_dir_all(nested.join(".chaos")).await?;
    tokio::fs::write(project_root.join(".git"), "gitdir: here").await?;

    let cases = [
        ("untrusted", Some(TrustLevel::Untrusted)),
        ("unknown", None),
    ];
    for (name, trust_level) in cases {
        let chaos_home = tmp.path().join(format!("home_{name}"));
        tokio::fs::create_dir_all(&chaos_home).await?;
        match trust_level {
            Some(trust_level) => {
                make_config_for_test(&chaos_home, &project_root, trust_level, None).await?;
            }
            None => bootstrap_sqlite(&chaos_home).map_err(std::io::Error::other)?,
        }
        chaos_sysctl::edit::ConfigEditsBuilder::new(&chaos_home)
            .set_model(Some("user"), None)
            .apply()
            .await
            .map_err(std::io::Error::other)?;

        let layers = load_layers(&chaos_home, &nested).await?;
        let project_folders: Vec<_> = layers
            .asset_folders()
            .iter()
            .filter(|folder| folder.scope == AssetScope::Project)
            .collect();
        assert_eq!(
            project_folders.len(),
            1,
            "expected one project folder for {name}"
        );
        assert_eq!(project_folders[0].dir.as_path(), nested.join(".chaos"));
        assert!(
            !project_folders[0].is_enabled(),
            "expected {name} project folder to be disabled"
        );
        assert!(
            project_folders[0]
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("rules")),
            "reason should explain what is skipped: {:?}",
            project_folders[0].disabled_reason
        );
        assert_eq!(
            layers.effective_config().get("model"),
            Some(&TomlValue::String("user".to_string()))
        );
    }

    Ok(())
}

#[tokio::test]
async fn project_root_dot_mcp_json_loads_when_project_is_trusted() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    let chaos_home = tmp.path().join("home");
    tokio::fs::create_dir_all(&nested).await?;
    tokio::fs::create_dir_all(&chaos_home).await?;
    tokio::fs::write(project_root.join(".git"), "gitdir: here").await?;
    tokio::fs::write(
        project_root.join(".mcp.json"),
        r#"
{
  "mcpServers": {
    "docs": {
      "command": "node",
      "args": ["server.js"]
    }
  }
}
"#,
    )
    .await?;
    make_config_for_test(&chaos_home, &project_root, TrustLevel::Trusted, None).await?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home)
        .fallback_cwd(Some(nested))
        .build()
        .await?;

    let server = config
        .mcp_servers
        .get()
        .get("docs")
        .expect("trusted project .mcp.json should load");
    std::assert_matches!(
        server.transport,
        crate::config::types::McpServerTransportConfig::Stdio { .. }
    );

    Ok(())
}

#[tokio::test]
async fn project_root_dot_mcp_json_is_ignored_when_project_is_untrusted() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    let chaos_home = tmp.path().join("home");
    tokio::fs::create_dir_all(&nested).await?;
    tokio::fs::create_dir_all(&chaos_home).await?;
    tokio::fs::write(project_root.join(".git"), "gitdir: here").await?;
    tokio::fs::write(
        project_root.join(".mcp.json"),
        r#"
{
  "mcpServers": {
    "docs": {
      "command": "node",
      "args": ["server.js"]
    }
  }
}
"#,
    )
    .await?;
    make_config_for_test(&chaos_home, &project_root, TrustLevel::Untrusted, None).await?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home)
        .fallback_cwd(Some(nested))
        .build()
        .await?;

    assert!(
        !config.mcp_servers.get().contains_key("docs"),
        "untrusted project .mcp.json should not affect effective MCP servers"
    );

    Ok(())
}

#[tokio::test]
async fn cli_override_for_project_local_mcp_server_is_rejected() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    let chaos_home = tmp.path().join("home");
    tokio::fs::create_dir_all(&nested).await?;
    tokio::fs::create_dir_all(&chaos_home).await?;
    tokio::fs::write(project_root.join(".git"), "gitdir: here").await?;
    bootstrap_sqlite(&chaos_home).map_err(std::io::Error::other)?;

    let err = ConfigBuilder::default()
        .chaos_home(chaos_home)
        .cli_overrides(vec![(
            "mcp_servers.sentry.enabled".to_string(),
            TomlValue::Boolean(true),
        )])
        .fallback_cwd(Some(nested))
        .build()
        .await
        .expect_err("expected error");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("top-level `mcp_servers`"));

    Ok(())
}

#[tokio::test]
async fn cli_overrides_with_relative_paths_do_not_break_trust_check() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    tokio::fs::create_dir_all(&nested).await?;
    tokio::fs::write(project_root.join(".git"), "gitdir: here").await?;

    let chaos_home = tmp.path().join("home");
    tokio::fs::create_dir_all(&chaos_home).await?;
    make_config_for_test(&chaos_home, &project_root, TrustLevel::Trusted, None).await?;

    let cwd = AbsolutePathBuf::from_absolute_path(&nested)?;
    let cli_overrides = vec![(
        "model_instructions_file".to_string(),
        TomlValue::String("relative.md".to_string()),
    )];

    load_config_layers_state(
        &chaos_home,
        Some(cwd),
        &cli_overrides,
        LoaderOverrides::default(),
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn project_root_markers_supports_alternate_markers() -> std::io::Result<()> {
    let tmp = tempdir()?;
    let project_root = tmp.path().join("project");
    let nested = project_root.join("child");
    tokio::fs::create_dir_all(project_root.join(".chaos")).await?;
    tokio::fs::create_dir_all(nested.join(".chaos")).await?;
    tokio::fs::write(project_root.join(".hg"), "hg").await?;

    let chaos_home = tmp.path().join("home");
    tokio::fs::create_dir_all(&chaos_home).await?;
    make_config_for_test(
        &chaos_home,
        &project_root,
        TrustLevel::Trusted,
        Some(vec![".hg".to_string()]),
    )
    .await?;

    let layers = load_layers(&chaos_home, &nested).await?;

    assert_eq!(
        project_asset_dirs(&layers),
        vec![project_root.join(".chaos"), nested.join(".chaos")],
        "the .hg marker must anchor the project root"
    );

    Ok(())
}

mod requirements_exec_policy_tests {
    use crate::config_loader::AssetFolder;
    use crate::config_loader::AssetScope;
    use crate::config_loader::ConfigLayerStack;
    use crate::config_loader::ConfigRequirements;
    use crate::config_loader::ConfigRequirementsToml;
    use crate::config_loader::ConfigRequirementsWithSources;
    use crate::config_loader::RequirementSource;
    use crate::exec_policy::load_exec_policy;
    use chaos_realpath::AbsolutePathBuf;
    use chaos_selinux::Decision;
    use chaos_selinux::Evaluation;
    use chaos_selinux::RuleMatch;
    use chaos_sysctl::RequirementsExecPolicyDecisionToml;
    use chaos_sysctl::RequirementsExecPolicyParseError;
    use chaos_sysctl::RequirementsExecPolicyPatternTokenToml;
    use chaos_sysctl::RequirementsExecPolicyPrefixRuleToml;
    use chaos_sysctl::RequirementsExecPolicyToml;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use tempfile::tempdir;
    use toml::from_str;

    fn tokens(cmd: &[&str]) -> Vec<String> {
        cmd.iter().map(std::string::ToString::to_string).collect()
    }

    fn panic_if_called(_: &[String]) -> Decision {
        panic!("rule should match so heuristic should not be called");
    }

    fn config_stack_for_dot_codex_folder_with_requirements(
        dot_codex_folder: &Path,
        requirements: ConfigRequirements,
    ) -> ConfigLayerStack {
        let dot_codex_folder = AbsolutePathBuf::from_absolute_path(dot_codex_folder)
            .expect("absolute dot_codex_folder");
        ConfigLayerStack::new(Vec::new(), requirements, ConfigRequirementsToml::default())
            .expect("ConfigLayerStack")
            .with_asset_folders(vec![AssetFolder::new(
                AssetScope::Project,
                dot_codex_folder,
            )])
    }

    fn requirements_from_toml(toml_str: &str) -> ConfigRequirements {
        let config: ConfigRequirementsToml = from_str(toml_str).expect("parse requirements toml");
        let mut with_sources = ConfigRequirementsWithSources::default();
        with_sources.merge_unset_fields(RequirementSource::Unknown, config);
        ConfigRequirements::try_from(with_sources).expect("requirements")
    }

    #[test]
    fn parses_single_prefix_rule_from_raw_toml() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "forbidden" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;

        assert_eq!(
            parsed,
            RequirementsExecPolicyToml {
                prefix_rules: vec![RequirementsExecPolicyPrefixRuleToml {
                    pattern: vec![RequirementsExecPolicyPatternTokenToml {
                        token: Some("rm".to_string()),
                        any_of: None,
                    }],
                    decision: Some(RequirementsExecPolicyDecisionToml::Forbidden),
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[test]
    fn parses_multiple_prefix_rules_from_raw_toml() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "forbidden" },
    { pattern = [{ token = "git" }, { any_of = ["push", "commit"] }], decision = "prompt", justification = "review changes before push or commit" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;

        assert_eq!(
            parsed,
            RequirementsExecPolicyToml {
                prefix_rules: vec![
                    RequirementsExecPolicyPrefixRuleToml {
                        pattern: vec![RequirementsExecPolicyPatternTokenToml {
                            token: Some("rm".to_string()),
                            any_of: None,
                        }],
                        decision: Some(RequirementsExecPolicyDecisionToml::Forbidden),
                        justification: None,
                    },
                    RequirementsExecPolicyPrefixRuleToml {
                        pattern: vec![
                            RequirementsExecPolicyPatternTokenToml {
                                token: Some("git".to_string()),
                                any_of: None,
                            },
                            RequirementsExecPolicyPatternTokenToml {
                                token: None,
                                any_of: Some(vec!["push".to_string(), "commit".to_string()]),
                            },
                        ],
                        decision: Some(RequirementsExecPolicyDecisionToml::Prompt),
                        justification: Some("review changes before push or commit".to_string()),
                    },
                ],
            }
        );

        Ok(())
    }

    #[test]
    fn converts_rules_toml_into_internal_policy_representation() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "forbidden" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let policy = parsed.to_policy()?;

        assert_eq!(
            policy.check(&tokens(&["rm", "-rf", "/tmp"]), &panic_if_called),
            Evaluation {
                decision: Decision::Forbidden,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: tokens(&["rm"]),
                    decision: Decision::Forbidden,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[test]
    fn head_any_of_expands_into_multiple_program_rules() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ any_of = ["git", "hg"] }, { token = "status" }], decision = "prompt" },
]
"#;
        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let policy = parsed.to_policy()?;

        assert_eq!(
            policy.check(&tokens(&["git", "status"]), &panic_if_called),
            Evaluation {
                decision: Decision::Prompt,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: tokens(&["git", "status"]),
                    decision: Decision::Prompt,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );
        assert_eq!(
            policy.check(&tokens(&["hg", "status"]), &panic_if_called),
            Evaluation {
                decision: Decision::Prompt,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: tokens(&["hg", "status"]),
                    decision: Decision::Prompt,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[test]
    fn missing_decision_is_rejected() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }] },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let err = parsed.to_policy().expect_err("missing decision");

        std::assert_matches!(
            err,
            RequirementsExecPolicyParseError::MissingDecision { rule_index: 0 }
        );
        Ok(())
    }

    #[test]
    fn allow_decision_is_rejected() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "allow" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let err = parsed.to_policy().expect_err("allow decision not allowed");

        std::assert_matches!(
            err,
            RequirementsExecPolicyParseError::AllowDecisionNotAllowed { rule_index: 0, .. }
        );
        Ok(())
    }

    #[test]
    fn empty_prefix_rules_is_rejected() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = []
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let err = parsed.to_policy().expect_err("empty prefix rules");

        std::assert_matches!(err, RequirementsExecPolicyParseError::EmptyPrefixRules);
        Ok(())
    }

    #[tokio::test]
    async fn loads_requirements_exec_policy_without_rules_files() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let requirements = requirements_from_toml(
            r#"
                [rules]
                prefix_rules = [
                    { pattern = [{ token = "rm" }], decision = "forbidden" },
                ]
            "#,
        );
        let config_stack =
            config_stack_for_dot_codex_folder_with_requirements(temp_dir.path(), requirements);

        let home = tempdir()?;
        crate::test_support::bootstrap_sqlite(home.path())?;
        let policy = load_exec_policy(&config_stack, home.path()).await?;

        assert_eq!(
            policy.check_multiple([vec!["rm".to_string()]].iter(), &panic_if_called),
            Evaluation {
                decision: Decision::Forbidden,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: vec!["rm".to_string()],
                    decision: Decision::Forbidden,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[tokio::test]
    async fn merges_requirements_exec_policy_with_file_rules() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let policy_dir = temp_dir.path().join("rules");
        std::fs::create_dir_all(&policy_dir)?;
        std::fs::write(
            policy_dir.join("deny.decrees"),
            r#"prefix_rule {pattern={"rm"}, decision="forbidden"}"#,
        )?;

        let requirements = requirements_from_toml(
            r#"
                [rules]
                prefix_rules = [
                    { pattern = [{ token = "git" }, { token = "push" }], decision = "prompt" },
                ]
            "#,
        );
        let config_stack =
            config_stack_for_dot_codex_folder_with_requirements(temp_dir.path(), requirements);

        let home = tempdir()?;
        crate::test_support::bootstrap_sqlite(home.path())?;
        let policy = load_exec_policy(&config_stack, home.path()).await?;

        assert_eq!(
            policy.check_multiple([vec!["rm".to_string()]].iter(), &panic_if_called),
            Evaluation {
                decision: Decision::Forbidden,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: vec!["rm".to_string()],
                    decision: Decision::Forbidden,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );
        assert_eq!(
            policy.check_multiple(
                [vec!["git".to_string(), "push".to_string()]].iter(),
                &panic_if_called
            ),
            Evaluation {
                decision: Decision::Prompt,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: vec!["git".to_string(), "push".to_string()],
                    decision: Decision::Prompt,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }
}
