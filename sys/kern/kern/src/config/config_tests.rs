use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;
use crate::config::edit::apply_blocking;
use crate::config::types::ApprovalsReviewer;
use crate::config::types::FeedbackConfigToml;
use crate::config::types::HistoryPersistence;
use crate::config::types::McpServerDisabledReason;
use crate::config::types::McpServerTransportConfig;
use crate::config::types::ModelAvailabilityNuxConfig;
use crate::config::types::NotificationMethod;
use crate::config::types::Notifications;
use crate::config_loader::McpServerIdentity;
use crate::config_loader::RequirementSource;

use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsAccessMode;
use chaos_ipc::permissions::VfsEntry;
use chaos_ipc::permissions::VfsPath;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::permissions::VfsSpecialPath;
use chaos_sysctl::CONFIG_TOML_FILE;
use tempfile::tempdir;

use super::*;
use core_test_support::test_absolute_path;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::time::Duration;
use tempfile::TempDir;
use toml_edit::DocumentMut;

#[path = "config_tests/agent_roles.rs"]
mod agent_roles;
#[path = "config_tests/compact_and_catalog.rs"]
mod compact_and_catalog;
#[path = "config_tests/mcp_and_shell.rs"]
mod mcp_and_shell;
#[path = "config_tests/mcp_servers.rs"]
mod mcp_servers;
#[path = "config_tests/model_and_feature_edits.rs"]
mod model_and_feature_edits;
#[path = "config_tests/oss_provider.rs"]
mod oss_provider;
#[path = "config_tests/permissions_profiles.rs"]
mod permissions_profiles;
#[path = "config_tests/project_trust.rs"]
mod project_trust;
#[path = "config_tests/realtime.rs"]
mod realtime;
#[path = "config_tests/requirements.rs"]
mod requirements;
#[path = "config_tests/sandbox_policy.rs"]
mod sandbox_policy;
#[path = "config_tests/tui.rs"]
mod tui;

#[test]
fn test_toml_parsing() {
    let history_with_persistence = r#"
[history]
persistence = "save-all"
"#;
    let history_with_persistence_cfg = toml::from_str::<ConfigToml>(history_with_persistence)
        .expect("TOML deserialization should succeed");
    assert_eq!(
        Some(History {
            persistence: HistoryPersistence::SaveAll,
            max_bytes: None,
        }),
        history_with_persistence_cfg.history
    );

    let history_no_persistence = r#"
[history]
persistence = "none"
"#;

    let history_no_persistence_cfg = toml::from_str::<ConfigToml>(history_no_persistence)
        .expect("TOML deserialization should succeed");
    assert_eq!(
        Some(History {
            persistence: HistoryPersistence::None,
            max_bytes: None,
        }),
        history_no_persistence_cfg.history
    );
}

#[test]
fn tools_web_search_true_deserializes_to_none() {
    let cfg: ConfigToml = toml::from_str(
        r#"
[tools]
web_search = true
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.tools,
        Some(ToolsToml {
            web_search: None,
            view_image: None,
        })
    );
}

#[test]
fn tools_web_search_false_deserializes_to_none() {
    let cfg: ConfigToml = toml::from_str(
        r#"
[tools]
web_search = false
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.tools,
        Some(ToolsToml {
            web_search: None,
            view_image: None,
        })
    );
}

#[test]
fn config_toml_deserializes_model_availability_nux() {
    let toml = r#"
[tui.model_availability_nux]
"serpent" = 2
"gordon" = 4
"#;
    let cfg: ConfigToml =
        toml::from_str(toml).expect("TOML deserialization should succeed for TUI NUX");

    assert_eq!(
        cfg.tui.expect("tui config should deserialize"),
        Tui {
            notifications: Notifications::default(),
            notification_method: NotificationMethod::default(),
            animations: true,
            alternate_screen: AltScreenMode::default(),
            status_line: None,
            theme: None,
            model_availability_nux: ModelAvailabilityNuxConfig {
                shown_count: HashMap::from(
                    [("gordon".to_string(), 4), ("serpent".to_string(), 2),]
                ),
            },
        }
    );
}

#[test]
fn runtime_config_defaults_model_availability_nux() {
    let cfg = Config::load_from_base_config_with_overrides(
        ConfigToml::default(),
        ConfigOverrides::default(),
        tempdir().expect("tempdir").path().to_path_buf(),
    )
    .expect("load config");

    assert_eq!(
        cfg.model_availability_nux,
        ModelAvailabilityNuxConfig::default()
    );
}

#[test]
fn tui_theme_deserializes_from_toml() {
    let cfg = r#"
[tui]
theme = "dracula"
"#;
    let parsed = toml::from_str::<ConfigToml>(cfg).expect("TOML deserialization should succeed");
    assert_eq!(
        parsed.tui.as_ref().and_then(|t| t.theme.as_deref()),
        Some("dracula"),
    );
}

#[test]
fn tui_theme_defaults_to_none() {
    let cfg = r#"
[tui]
"#;
    let parsed = toml::from_str::<ConfigToml>(cfg).expect("TOML deserialization should succeed");
    assert_eq!(parsed.tui.as_ref().and_then(|t| t.theme.as_deref()), None);
}

#[test]
fn tui_config_missing_notifications_field_defaults_to_enabled() {
    let cfg = r#"
[tui]
"#;

    let parsed =
        toml::from_str::<ConfigToml>(cfg).expect("TUI config without notifications should succeed");
    let tui = parsed.tui.expect("config should include tui section");

    assert_eq!(
        tui,
        Tui {
            notifications: Notifications::Enabled(true),
            notification_method: NotificationMethod::Auto,
            animations: true,
            alternate_screen: AltScreenMode::Auto,
            status_line: None,
            theme: None,
            model_availability_nux: ModelAvailabilityNuxConfig::default(),
        }
    );
}

#[test]
fn test_sandbox_config_parsing() {
    let sandbox_full_access = r#"
sandbox_mode = "root-access"

[sandbox_workspace_write]
network_access = false  # This should be ignored.
"#;
    let sandbox_full_access_cfg = toml::from_str::<ConfigToml>(sandbox_full_access)
        .expect("TOML deserialization should succeed");
    let sandbox_mode_override = None;
    let resolution = sandbox_full_access_cfg.derive_sandbox_policy(
        sandbox_mode_override,
        None,
        &PathBuf::from("/tmp/test"),
        None,
    );
    assert_eq!(resolution, SandboxPolicy::RootAccess);

    let sandbox_read_only = r#"
sandbox_mode = "read-only"

[sandbox_workspace_write]
network_access = true  # This should be ignored.
"#;

    let sandbox_read_only_cfg = toml::from_str::<ConfigToml>(sandbox_read_only)
        .expect("TOML deserialization should succeed");
    let sandbox_mode_override = None;
    let resolution = sandbox_read_only_cfg.derive_sandbox_policy(
        sandbox_mode_override,
        None,
        &PathBuf::from("/tmp/test"),
        None,
    );
    assert_eq!(resolution, SandboxPolicy::new_read_only_policy());

    let writable_root = test_absolute_path("/my/workspace");
    let sandbox_workspace_write = format!(
        r#"
sandbox_mode = "workspace-write"

[sandbox_workspace_write]
writable_roots = [
    {},
]
exclude_tmpdir_env_var = true
exclude_slash_tmp = true
"#,
        serde_json::json!(writable_root)
    );

    let sandbox_workspace_write_cfg = toml::from_str::<ConfigToml>(&sandbox_workspace_write)
        .expect("TOML deserialization should succeed");
    let sandbox_mode_override = None;
    let resolution = sandbox_workspace_write_cfg.derive_sandbox_policy(
        sandbox_mode_override,
        None,
        &PathBuf::from("/tmp/test"),
        None,
    );
    assert_eq!(
        resolution,
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![writable_root.clone()],
            read_only_access: ReadOnlyAccess::FullAccess,
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        }
    );

    let sandbox_workspace_write = format!(
        r#"
sandbox_mode = "workspace-write"

[sandbox_workspace_write]
writable_roots = [
    {},
]
exclude_tmpdir_env_var = true
exclude_slash_tmp = true

[projects."/tmp/test"]
trust_level = "trusted"
"#,
        serde_json::json!(writable_root)
    );

    let sandbox_workspace_write_cfg = toml::from_str::<ConfigToml>(&sandbox_workspace_write)
        .expect("TOML deserialization should succeed");
    let sandbox_mode_override = None;
    let resolution = sandbox_workspace_write_cfg.derive_sandbox_policy(
        sandbox_mode_override,
        None,
        &PathBuf::from("/tmp/test"),
        None,
    );
    assert_eq!(
        resolution,
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![writable_root],
            read_only_access: ReadOnlyAccess::FullAccess,
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        }
    );
}

#[test]
fn legacy_sandbox_mode_config_builds_split_policies_without_drift() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    let extra_root = test_absolute_path("/tmp/legacy-extra-root");
    let cases = vec![
        (
            "root-access".to_string(),
            r#"sandbox_mode = "root-access"
"#
            .to_string(),
        ),
        (
            "read-only".to_string(),
            r#"sandbox_mode = "read-only"
"#
            .to_string(),
        ),
        (
            "workspace-write".to_string(),
            format!(
                r#"sandbox_mode = "workspace-write"

[sandbox_workspace_write]
writable_roots = [{}]
exclude_tmpdir_env_var = true
exclude_slash_tmp = true
"#,
                serde_json::json!(extra_root)
            ),
        ),
    ];

    for (name, config_toml) in cases {
        let cfg = toml::from_str::<ConfigToml>(&config_toml)
            .unwrap_or_else(|err| panic!("case `{name}` should parse: {err}"));
        let config = Config::load_from_base_config_with_overrides(
            cfg,
            ConfigOverrides {
                cwd: Some(cwd.path().to_path_buf()),
                ..Default::default()
            },
            chaos_home.path().to_path_buf(),
        )?;

        let sandbox_policy = config.permissions.sandbox_policy.get();
        assert_eq!(
            config.permissions.vfs_policy,
            VfsPolicy::from_sandbox_policy(sandbox_policy, cwd.path()),
            "case `{name}` should preserve filesystem semantics from sandbox config"
        );
        assert_eq!(
            config.permissions.socket_policy,
            SocketPolicy::from(sandbox_policy),
            "case `{name}` should preserve network semantics from legacy config"
        );
        assert_eq!(
            config
                .permissions
                .vfs_policy
                .to_sandbox_policy(config.permissions.socket_policy, cwd.path())
                .unwrap_or_else(|err| panic!("case `{name}` should round-trip: {err}")),
            sandbox_policy.clone(),
            "case `{name}` should round-trip through split policies without drift"
        );
    }

    Ok(())
}

#[test]
fn add_dir_override_extends_workspace_writable_roots() -> std::io::Result<()> {
    let temp_dir = TempDir::new()?;
    let frontend = temp_dir.path().join("frontend");
    let backend = temp_dir.path().join("backend");
    std::fs::create_dir_all(&frontend)?;
    std::fs::create_dir_all(&backend)?;

    let overrides = ConfigOverrides {
        cwd: Some(frontend),
        sandbox_mode: Some(SandboxMode::WorkspaceWrite),
        additional_writable_roots: vec![PathBuf::from("../backend"), backend.clone()],
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml::default(),
        overrides,
        temp_dir.path().to_path_buf(),
    )?;

    let expected_backend = AbsolutePathBuf::try_from(backend).unwrap();
    match config.permissions.sandbox_policy.get() {
        SandboxPolicy::WorkspaceWrite { writable_roots, .. } => {
            assert_eq!(
                writable_roots
                    .iter()
                    .filter(|root| **root == expected_backend)
                    .count(),
                1,
                "expected single writable root entry for {}",
                expected_backend.display()
            );
        }
        other => panic!("expected workspace-write policy, got {other:?}"),
    }

    Ok(())
}

#[test]
fn sqlite_home_defaults_to_codex_home_for_workspace_write() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        ConfigToml::default(),
        ConfigOverrides {
            sandbox_mode: Some(SandboxMode::WorkspaceWrite),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(config.sqlite_home, chaos_home.path().to_path_buf());

    Ok(())
}

#[test]
fn workspace_write_includes_explicit_writable_root_once() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let extra_root = chaos_home.path().join("extra");
    std::fs::create_dir_all(&extra_root)?;
    let config = Config::load_from_base_config_with_overrides(
        ConfigToml {
            sandbox_workspace_write: Some(SandboxWorkspaceWrite {
                writable_roots: vec![AbsolutePathBuf::from_absolute_path(&extra_root)?],
                ..Default::default()
            }),
            ..Default::default()
        },
        ConfigOverrides {
            sandbox_mode: Some(SandboxMode::WorkspaceWrite),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )?;

    let expected_root = AbsolutePathBuf::from_absolute_path(&extra_root)?;
    match config.permissions.sandbox_policy.get() {
        SandboxPolicy::WorkspaceWrite { writable_roots, .. } => {
            assert_eq!(
                writable_roots
                    .iter()
                    .filter(|root| **root == expected_root)
                    .count(),
                1,
                "expected single writable root entry for {}",
                expected_root.display()
            );
        }
        other => panic!("expected workspace-write policy, got {other:?}"),
    }

    Ok(())
}

#[test]
fn config_defaults_to_file_cli_auth_store_mode() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cfg = ConfigToml::default();

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.cli_auth_credentials_store_mode,
        AuthCredentialsStoreMode::File,
    );

    Ok(())
}

#[test]
fn config_honors_explicit_keyring_auth_store_mode() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cfg = ConfigToml {
        cli_auth_credentials_store: Some(AuthCredentialsStoreMode::Keyring),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.cli_auth_credentials_store_mode,
        AuthCredentialsStoreMode::Keyring,
    );

    Ok(())
}

#[test]
fn config_defaults_to_auto_oauth_store_mode() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cfg = ConfigToml::default();

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.mcp_oauth_credentials_store_mode,
        OAuthCredentialsStoreMode::Auto,
    );

    Ok(())
}

#[test]
fn feedback_enabled_defaults_to_true() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cfg = ConfigToml {
        feedback: Some(FeedbackConfigToml::default()),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert!(config.feedback_enabled);

    Ok(())
}

#[test]
fn web_search_mode_defaults_to_none_if_unset() {
    let cfg = ConfigToml::default();
    let profile = ConfigProfile::default();
    let features = Features::with_defaults();

    assert_eq!(resolve_web_search_mode(&cfg, &profile, &features), None);
}

#[test]
fn web_search_mode_prefers_profile_over_config() {
    let cfg = ConfigToml::default();
    let profile = ConfigProfile {
        web_search: Some(WebSearchMode::Live),
        ..Default::default()
    };
    let features = Features::with_defaults();

    assert_eq!(
        resolve_web_search_mode(&cfg, &profile, &features),
        Some(WebSearchMode::Live)
    );
}

#[test]
fn web_search_mode_disabled_from_config() {
    let cfg = ConfigToml {
        web_search: Some(WebSearchMode::Disabled),
        ..Default::default()
    };
    let profile = ConfigProfile::default();
    let features = Features::with_defaults();

    assert_eq!(
        resolve_web_search_mode(&cfg, &profile, &features),
        Some(WebSearchMode::Disabled)
    );
}

#[test]
fn web_search_mode_for_turn_uses_preference_for_read_only() {
    let web_search_mode = Constrained::allow_any(WebSearchMode::Cached);
    let mode = resolve_web_search_mode_for_turn(
        &web_search_mode,
        &VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
    );

    assert_eq!(mode, WebSearchMode::Cached);
}

#[test]
fn web_search_mode_for_turn_prefers_live_for_root_access() {
    let web_search_mode = Constrained::allow_any(WebSearchMode::Cached);
    let mode = resolve_web_search_mode_for_turn(&web_search_mode, &VfsPolicy::unrestricted());

    assert_eq!(mode, WebSearchMode::Live);
}

#[test]
fn web_search_mode_for_turn_respects_disabled_for_root_access() {
    let web_search_mode = Constrained::allow_any(WebSearchMode::Disabled);
    let mode = resolve_web_search_mode_for_turn(&web_search_mode, &VfsPolicy::unrestricted());

    assert_eq!(mode, WebSearchMode::Disabled);
}

#[test]
fn web_search_mode_for_turn_falls_back_when_live_is_disallowed() -> anyhow::Result<()> {
    let allowed = [WebSearchMode::Disabled, WebSearchMode::Cached];
    let web_search_mode = Constrained::new(WebSearchMode::Cached, move |candidate| {
        if allowed.contains(candidate) {
            Ok(())
        } else {
            Err(ConstraintError::InvalidValue {
                field_name: "web_search_mode",
                candidate: format!("{candidate:?}"),
                allowed: format!("{allowed:?}"),
                requirement_source: RequirementSource::Unknown,
            })
        }
    })?;
    let mode = resolve_web_search_mode_for_turn(&web_search_mode, &VfsPolicy::unrestricted());

    assert_eq!(mode, WebSearchMode::Cached);
    Ok(())
}

#[tokio::test]
async fn project_profile_overrides_user_profile() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    let workspace_key = workspace.path().to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        format!(
            r#"
profile = "global"

[profiles.global]
model = "serpent"

[profiles.project]
model = "gordon"

[projects."{workspace_key}"]
trust_level = "trusted"
"#,
        ),
    )?;
    let project_config_dir = workspace.path().join(".chaos");
    std::fs::create_dir_all(&project_config_dir)?;
    std::fs::write(
        project_config_dir.join(CONFIG_TOML_FILE),
        r#"
profile = "project"
"#,
    )?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(workspace.path().to_path_buf()),
            ..Default::default()
        })
        .build()
        .await?;

    assert_eq!(config.active_profile.as_deref(), Some("project"));
    assert_eq!(config.model.as_deref(), Some("gordon"));

    Ok(())
}

#[test]
fn profile_sandbox_mode_overrides_base() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let mut profiles = HashMap::new();
    profiles.insert(
        "work".to_string(),
        ConfigProfile {
            sandbox_mode: Some(SandboxMode::RootAccess),
            ..Default::default()
        },
    );
    let cfg = ConfigToml {
        profiles,
        profile: Some("work".to_string()),
        sandbox_mode: Some(SandboxMode::ReadOnly),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert!(matches!(
        config.permissions.sandbox_policy.get(),
        &SandboxPolicy::RootAccess
    ));

    Ok(())
}

#[test]
fn cli_override_takes_precedence_over_profile_sandbox_mode() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let mut profiles = HashMap::new();
    profiles.insert(
        "work".to_string(),
        ConfigProfile {
            sandbox_mode: Some(SandboxMode::RootAccess),
            ..Default::default()
        },
    );
    let cfg = ConfigToml {
        profiles,
        profile: Some("work".to_string()),
        ..Default::default()
    };

    let overrides = ConfigOverrides {
        sandbox_mode: Some(SandboxMode::WorkspaceWrite),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        overrides,
        chaos_home.path().to_path_buf(),
    )?;

    assert!(matches!(
        config.permissions.sandbox_policy.get(),
        SandboxPolicy::WorkspaceWrite { .. }
    ));

    Ok(())
}

#[test]
fn responses_websocket_features_do_not_change_wire_api() -> std::io::Result<()> {
    for feature_key in ["responses_websockets", "responses_websockets_v2"] {
        let chaos_home = TempDir::new()?;
        let mut entries = BTreeMap::new();
        entries.insert(feature_key.to_string(), true);
        let cfg = ConfigToml {
            features: Some(crate::features::FeaturesToml { entries }),
            ..Default::default()
        };

        let config = Config::load_from_base_config_with_overrides(
            cfg,
            ConfigOverrides::default(),
            chaos_home.path().to_path_buf(),
        )?;

        assert_eq!(
            config.model_provider.wire_api,
            crate::model_provider_info::WireApi::Responses
        );
    }

    Ok(())
}

#[test]
fn config_honors_explicit_file_oauth_store_mode() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cfg = ConfigToml {
        mcp_oauth_credentials_store: Some(OAuthCredentialsStoreMode::File),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.mcp_oauth_credentials_store_mode,
        OAuthCredentialsStoreMode::File,
    );

    Ok(())
}

struct PrecedenceTestFixture {
    cwd: TempDir,
    chaos_home: TempDir,
    cfg: ConfigToml,
    model_provider_map: HashMap<String, ModelProviderInfo>,
    openai_provider: ModelProviderInfo,
    openai_custom_provider: ModelProviderInfo,
}

impl PrecedenceTestFixture {
    fn cwd(&self) -> PathBuf {
        self.cwd.path().to_path_buf()
    }

    fn chaos_home(&self) -> PathBuf {
        self.chaos_home.path().to_path_buf()
    }
}

fn load_precedence_fixture_config(
    fixture: &PrecedenceTestFixture,
    profile: Option<&str>,
) -> std::io::Result<Config> {
    Config::load_from_base_config_with_overrides(
        fixture.cfg.clone(),
        ConfigOverrides {
            config_profile: profile.map(str::to_owned),
            cwd: Some(fixture.cwd()),
            ..Default::default()
        },
        fixture.chaos_home(),
    )
}

fn expected_precedence_fixture_permissions(approval_policy: ApprovalPolicy) -> Permissions {
    Permissions {
        approval_policy: Constrained::allow_any(approval_policy),
        sandbox_policy: Constrained::allow_any(SandboxPolicy::new_read_only_policy()),
        vfs_policy: VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
        socket_policy: SocketPolicy::Restricted,
        network: None,
        allow_login_shell: true,
        shell_environment_policy: ShellEnvironmentPolicy::default(),
        macos_seatbelt_profile_extensions: None,
    }
}

fn expected_precedence_fixture_config_baseline(fixture: &PrecedenceTestFixture) -> Config {
    Config {
        model: Some("o3".to_string()),
        review_model: None,
        model_context_window: None,
        model_auto_compact_token_limit: None,
        service_tier: None,
        model_provider_id: "openai".to_string(),
        model_provider: fixture.openai_provider.clone(),
        permissions: expected_precedence_fixture_permissions(ApprovalPolicy::Supervised),
        approvals_reviewer: ApprovalsReviewer::User,
        enforce_residency: Constrained::allow_any(None),
        user_instructions: None,
        notify: None,
        cwd: fixture.cwd(),
        cli_auth_credentials_store_mode: Default::default(),
        mcp_servers: Constrained::allow_any(HashMap::new()),
        mcp_oauth_credentials_store_mode: Default::default(),
        mcp_oauth_callback_port: None,
        mcp_oauth_callback_url: None,
        model_providers: fixture.model_provider_map.clone(),
        tool_output_token_limit: None,
        agent_max_threads: DEFAULT_AGENT_MAX_THREADS,
        agent_max_depth: DEFAULT_AGENT_MAX_DEPTH,
        agent_roles: BTreeMap::new(),
        memories: MemoriesConfig::default(),
        minion_job_max_runtime_seconds: DEFAULT_MINION_JOB_MAX_RUNTIME_SECONDS,
        chaos_home: fixture.chaos_home(),
        sqlite_home: fixture.chaos_home(),
        log_dir: fixture.chaos_home().join("log"),
        config_layer_stack: Default::default(),
        startup_warnings: Vec::new(),
        history: History::default(),
        ephemeral: false,
        file_opener: UriBasedFileOpener::VsCode,
        alcatraz_linux_exe: None,
        alcatraz_freebsd_exe: None,
        alcatraz_macos_exe: None,
        hide_agent_reasoning: false,
        show_raw_agent_reasoning: false,
        model_reasoning_effort: None,
        plan_mode_reasoning_effort: None,
        model_reasoning_summary: None,
        model_supports_reasoning_summaries: None,
        model_catalog: None,
        model_verbosity: None,
        personality: Some(Personality::Pragmatic),
        chatgpt_base_url: "https://chatgpt.com/backend-api/".to_string(),
        realtime_audio: RealtimeAudioConfig::default(),
        experimental_realtime_start_instructions: None,
        experimental_realtime_ws_base_url: None,
        experimental_realtime_ws_model: None,
        realtime: RealtimeConfig::default(),
        experimental_realtime_ws_backend_prompt: None,
        experimental_realtime_ws_startup_context: None,
        base_instructions: None,
        minion_instructions: None,
        compact_prompt: None,
        forced_chatgpt_workspace_id: None,
        forced_login_method: None,
        web_search_mode: Constrained::allow_any(WebSearchMode::Cached),
        web_search_config: None,
        collab_enabled: true,
        background_terminal_max_timeout: DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS,
        ghost_snapshot: GhostSnapshotConfig::default(),
        features: Features::with_defaults().into(),
        active_profile: None,
        active_project: ProjectConfig { trust_level: None },
        notices: Default::default(),
        disable_paste_burst: false,
        tui_notifications: Default::default(),
        tui_notification_method: Default::default(),
        animations: true,
        model_availability_nux: ModelAvailabilityNuxConfig::default(),
        analytics_enabled: Some(true),
        feedback_enabled: true,
        tui_alternate_screen: AltScreenMode::Auto,
        tui_status_line: None,
        tui_theme: None,
        otel: OtelConfig::default(),
        disable_user_scripts: false,
    }
}

fn expected_precedence_fixture_profile_config(
    fixture: &PrecedenceTestFixture,
    profile: &str,
    apply_deltas: impl FnOnce(&mut Config),
) -> Config {
    let mut expected = expected_precedence_fixture_config_baseline(fixture);
    expected.active_profile = Some(profile.to_string());
    apply_deltas(&mut expected);
    expected
}

fn create_test_fixture() -> std::io::Result<PrecedenceTestFixture> {
    let toml = r#"
model = "o3"
approval_policy = "supervised"

# Can be used to determine which profile to use if not specified by
# `ConfigOverrides`.
profile = "serpent"

[analytics]
enabled = true

[model_providers.openai-custom]
name = "OpenAI custom"
base_url = "https://api.openai.com/v1"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
request_max_retries = 4            # retry failed HTTP requests
stream_max_retries = 10            # retry dropped SSE streams
stream_idle_timeout_ms = 300000    # 5m idle timeout

[profiles.o3]
model = "o3"
model_provider = "openai"
approval_policy = "headless"
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"

[profiles.serpent]
model = "serpent"
model_provider = "openai-custom"

[profiles.zdr]
model = "o3"
model_provider = "openai"
approval_policy = "interactive"

[profiles.zdr.analytics]
enabled = false

[profiles.gordon]
model = "gordon"
model_provider = "openai"
approval_policy = "interactive"
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"
model_verbosity = "high"
"#;

    let cfg: ConfigToml = toml::from_str(toml).expect("TOML deserialization should succeed");

    // Use a temporary directory for the cwd so it does not contain an
    // AGENTS.md file.
    let cwd_temp_dir = TempDir::new().unwrap();
    let cwd = cwd_temp_dir.path().to_path_buf();
    // Make it look like a Git repo so it does not search for AGENTS.md in
    // a parent folder, either.
    std::fs::write(cwd.join(".git"), "gitdir: nowhere")?;

    let codex_home_temp_dir = TempDir::new().unwrap();

    let openai_custom_provider = ModelProviderInfo {
        name: "OpenAI custom".to_string(),
        base_url: Some("https://api.openai.com/v1".to_string()),
        env_key: Some("OPENAI_API_KEY".to_string()),
        wire_api: crate::WireApi::Responses,
        env_key_instructions: None,
        experimental_bearer_token: None,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: Some(4),
        stream_max_retries: Some(10),
        stream_idle_timeout_ms: Some(300_000),
        requires_openai_auth: false,
        supports_websockets: false,
        native_server_side_tools: vec![],
    };
    let model_provider_map = {
        let mut model_provider_map = built_in_model_providers();
        model_provider_map.insert("openai-custom".to_string(), openai_custom_provider.clone());
        model_provider_map
    };

    let openai_provider = model_provider_map
        .get("openai")
        .expect("openai provider should exist")
        .clone();

    Ok(PrecedenceTestFixture {
        cwd: cwd_temp_dir,
        chaos_home: codex_home_temp_dir,
        cfg,
        model_provider_map,
        openai_provider,
        openai_custom_provider,
    })
}

/// Users can specify config values at multiple levels that have the
/// following precedence:
///
/// 1. custom command-line argument, e.g. `--model o3`
/// 2. as part of a profile, where the `--profile` is specified via a CLI
///    (or in the config file itself)
/// 3. as an entry in `config.toml`, e.g. `model = "o3"`
/// 4. the default value for a required field defined in code, e.g.,
///    `crate::flags::OPENAI_DEFAULT_MODEL`
///
/// Note that profiles are the recommended way to specify a group of
/// configuration options together.
#[test]
fn test_precedence_fixture_with_o3_profile() -> std::io::Result<()> {
    let fixture = create_test_fixture()?;

    let o3_profile_config = load_precedence_fixture_config(&fixture, Some("o3"))?;
    let expected_o3_profile_config =
        expected_precedence_fixture_profile_config(&fixture, "o3", |config| {
            config.permissions = expected_precedence_fixture_permissions(ApprovalPolicy::Headless);
            config.model_reasoning_effort = Some(ReasoningEffort::High);
            config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
        });

    assert_eq!(expected_o3_profile_config, o3_profile_config);
    Ok(())
}

#[test]
fn metrics_exporter_defaults_to_none_when_missing() -> std::io::Result<()> {
    let fixture = create_test_fixture()?;

    let config = Config::load_from_base_config_with_overrides(
        fixture.cfg.clone(),
        ConfigOverrides {
            cwd: Some(fixture.cwd()),
            ..Default::default()
        },
        fixture.chaos_home(),
    )?;

    assert_eq!(config.otel.metrics_exporter, OtelExporterKind::None);
    Ok(())
}

#[test]
fn test_precedence_fixture_with_serpent_profile() -> std::io::Result<()> {
    let fixture = create_test_fixture()?;

    let serpent_profile_config = load_precedence_fixture_config(&fixture, Some("serpent"))?;
    let expected_serpent_profile_config =
        expected_precedence_fixture_profile_config(&fixture, "serpent", |config| {
            config.model = Some("serpent".to_string());
            config.model_provider_id = "openai-custom".to_string();
            config.model_provider = fixture.openai_custom_provider.clone();
        });

    assert_eq!(expected_serpent_profile_config, serpent_profile_config);

    // Verify that loading without specifying a profile in ConfigOverrides
    // uses the default profile from the config file (which is "serpent").
    let default_profile_config = load_precedence_fixture_config(&fixture, None)?;

    assert_eq!(expected_serpent_profile_config, default_profile_config);
    Ok(())
}

#[test]
fn test_precedence_fixture_with_zdr_profile() -> std::io::Result<()> {
    let fixture = create_test_fixture()?;

    let zdr_profile_config = load_precedence_fixture_config(&fixture, Some("zdr"))?;
    let expected_zdr_profile_config =
        expected_precedence_fixture_profile_config(&fixture, "zdr", |config| {
            config.permissions =
                expected_precedence_fixture_permissions(ApprovalPolicy::Interactive);
            config.analytics_enabled = Some(false);
        });

    assert_eq!(expected_zdr_profile_config, zdr_profile_config);

    Ok(())
}

#[test]
fn test_precedence_fixture_with_gordon_profile() -> std::io::Result<()> {
    let fixture = create_test_fixture()?;

    let gordon_profile_config = load_precedence_fixture_config(&fixture, Some("gordon"))?;
    let expected_gordon_profile_config =
        expected_precedence_fixture_profile_config(&fixture, "gordon", |config| {
            config.model = Some("gordon".to_string());
            config.permissions =
                expected_precedence_fixture_permissions(ApprovalPolicy::Interactive);
            config.model_reasoning_effort = Some(ReasoningEffort::High);
            config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
            config.model_verbosity = Some(Verbosity::High);
        });

    assert_eq!(expected_gordon_profile_config, gordon_profile_config);

    Ok(())
}

#[test]
fn test_requirements_web_search_mode_allowlist_does_not_warn_when_unset() -> anyhow::Result<()> {
    let fixture = create_test_fixture()?;

    let requirements_toml = crate::config_loader::ConfigRequirementsToml {
        allowed_approval_policies: None,
        allowed_sandbox_modes: None,
        allowed_web_search_modes: Some(vec![
            crate::config_loader::WebSearchModeRequirement::Cached,
        ]),
        feature_requirements: None,
        mcp_servers: None,
        apps: None,
        rules: None,
        enforce_residency: None,
        network: None,
    };
    let requirement_source = crate::config_loader::RequirementSource::Unknown;
    let requirement_source_for_error = requirement_source.clone();
    let allowed = vec![WebSearchMode::Disabled, WebSearchMode::Cached];
    let constrained = Constrained::new(WebSearchMode::Cached, move |candidate| {
        if matches!(candidate, WebSearchMode::Cached | WebSearchMode::Disabled) {
            Ok(())
        } else {
            Err(ConstraintError::InvalidValue {
                field_name: "web_search_mode",
                candidate: format!("{candidate:?}"),
                allowed: format!("{allowed:?}"),
                requirement_source: requirement_source_for_error.clone(),
            })
        }
    })?;
    let requirements = crate::config_loader::ConfigRequirements {
        web_search_mode: crate::config_loader::ConstrainedWithSource::new(
            constrained,
            Some(requirement_source),
        ),
        ..Default::default()
    };
    let config_layer_stack =
        crate::config_loader::ConfigLayerStack::new(Vec::new(), requirements, requirements_toml)
            .expect("config layer stack");

    let config = Config::load_config_with_layer_stack(
        fixture.cfg.clone(),
        ConfigOverrides {
            cwd: Some(fixture.cwd()),
            ..Default::default()
        },
        fixture.chaos_home(),
        config_layer_stack,
    )?;

    assert!(
        !config
            .startup_warnings
            .iter()
            .any(|warning| warning.contains("Configured value for `web_search_mode`")),
        "{:?}",
        config.startup_warnings
    );

    Ok(())
}
