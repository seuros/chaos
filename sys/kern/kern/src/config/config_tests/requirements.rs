use super::*;

#[test]
fn test_untrusted_project_gets_unless_trusted_approval_policy() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    let test_project_dir = TempDir::new()?;
    let test_path = test_project_dir.path();

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml {
            projects: Some(HashMap::from([(
                test_path.to_string_lossy().to_string(),
                ProjectConfig {
                    trust_level: Some(TrustLevel::Untrusted),
                },
            )])),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(test_path.to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.permissions.approval_policy.value(),
        ApprovalPolicy::Supervised,
        "Expected Supervised approval policy for untrusted project"
    );

    assert!(
        matches!(
            config.permissions.sandbox_policy.get(),
            SandboxPolicy::WorkspaceWrite { .. }
        ),
        "Expected WorkspaceWrite sandbox for untrusted project"
    );

    Ok(())
}

#[tokio::test]
async fn requirements_disallowing_default_sandbox_falls_back_to_required_default()
-> std::io::Result<()> {
    let chaos_home = TempDir::new()?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                allowed_sandbox_modes: Some(vec![
                    crate::config_loader::SandboxModeRequirement::ReadOnly,
                ]),
                ..Default::default()
            }))
        }))
        .build()
        .await?;
    assert_eq!(
        *config.permissions.sandbox_policy.get(),
        SandboxPolicy::new_read_only_policy()
    );
    Ok(())
}

#[tokio::test]
async fn explicit_sandbox_mode_falls_back_when_disallowed_by_requirements() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    std::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        r#"sandbox_mode = "root-access"
"#,
    )?;

    let requirements = crate::config_loader::ConfigRequirementsToml {
        allowed_approval_policies: None,
        allowed_sandbox_modes: Some(vec![crate::config_loader::SandboxModeRequirement::ReadOnly]),
        allowed_web_search_modes: None,
        feature_requirements: None,
        mcp_servers: None,
        apps: None,
        rules: None,
        enforce_residency: None,
        network: None,
    };

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .cloud_requirements(CloudRequirementsLoader::new(async move {
            Ok(Some(requirements))
        }))
        .build()
        .await?;
    assert_eq!(
        *config.permissions.sandbox_policy.get(),
        SandboxPolicy::new_read_only_policy()
    );
    Ok(())
}

#[tokio::test]
async fn requirements_web_search_mode_overrides_root_access_default() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    std::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        r#"sandbox_mode = "root-access"
"#,
    )?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                allowed_web_search_modes: Some(vec![
                    crate::config_loader::WebSearchModeRequirement::Cached,
                ]),
                ..Default::default()
            }))
        }))
        .build()
        .await?;

    assert_eq!(config.web_search_mode.value(), WebSearchMode::Cached);
    assert_eq!(
        resolve_web_search_mode_for_turn(
            &config.web_search_mode,
            config.permissions.sandbox_policy.get(),
        ),
        WebSearchMode::Cached,
    );
    Ok(())
}

#[tokio::test]
async fn requirements_disallowing_default_approval_falls_back_to_required_default()
-> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    let workspace_key = workspace.path().to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        format!(
            r#"
[projects."{workspace_key}"]
trust_level = "untrusted"
"#
        ),
    )?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(workspace.path().to_path_buf()))
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                allowed_approval_policies: Some(vec![ApprovalPolicy::Interactive]),
                ..Default::default()
            }))
        }))
        .build()
        .await?;

    assert_eq!(
        config.permissions.approval_policy.value(),
        ApprovalPolicy::Interactive
    );
    Ok(())
}

#[tokio::test]
async fn explicit_approval_policy_falls_back_when_disallowed_by_requirements() -> std::io::Result<()>
{
    let chaos_home = TempDir::new()?;
    std::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        r#"approval_policy = "supervised"
"#,
    )?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                allowed_approval_policies: Some(vec![ApprovalPolicy::Interactive]),
                ..Default::default()
            }))
        }))
        .build()
        .await?;
    assert_eq!(
        config.permissions.approval_policy.value(),
        ApprovalPolicy::Interactive
    );
    Ok(())
}

#[tokio::test]
async fn feature_requirements_normalize_effective_feature_values() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                feature_requirements: Some(crate::config_loader::FeatureRequirementsToml {
                    entries: BTreeMap::from([
                        ("personality".to_string(), true),
                        ("shell_tool".to_string(), false),
                    ]),
                }),
                ..Default::default()
            }))
        }))
        .build()
        .await?;

    assert!(config.features.enabled(Feature::Personality));
    assert!(!config.features.enabled(Feature::ShellTool));
    assert!(
        !config
            .startup_warnings
            .iter()
            .any(|warning| warning.contains("Configured value for `features`")),
        "{:?}",
        config.startup_warnings
    );

    Ok(())
}

#[tokio::test]
async fn explicit_feature_config_is_normalized_by_requirements() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    std::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        r#"
[features]
personality = false
shell_tool = true
"#,
    )?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                feature_requirements: Some(crate::config_loader::FeatureRequirementsToml {
                    entries: BTreeMap::from([
                        ("personality".to_string(), true),
                        ("shell_tool".to_string(), false),
                    ]),
                }),
                ..Default::default()
            }))
        }))
        .build()
        .await?;

    assert!(config.features.enabled(Feature::Personality));
    assert!(!config.features.enabled(Feature::ShellTool));
    assert!(
        !config
            .startup_warnings
            .iter()
            .any(|warning| warning.contains("Configured value for `features`")),
        "{:?}",
        config.startup_warnings
    );

    Ok(())
}

#[tokio::test]
async fn approvals_reviewer_defaults_to_manual_only_without_guardian_feature() -> std::io::Result<()>
{
    let chaos_home = TempDir::new()?;

    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .build()
        .await?;

    assert_eq!(config.approvals_reviewer, ApprovalsReviewer::User);
    Ok(())
}

#[tokio::test]
async fn feature_requirements_normalize_runtime_feature_mutations() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;

    let mut config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                feature_requirements: Some(crate::config_loader::FeatureRequirementsToml {
                    entries: BTreeMap::from([
                        ("personality".to_string(), true),
                        ("shell_tool".to_string(), false),
                    ]),
                }),
                ..Default::default()
            }))
        }))
        .build()
        .await?;

    let mut requested = config.features.get().clone();
    requested
        .disable(Feature::Personality)
        .enable(Feature::ShellTool);
    assert!(config.features.can_set(&requested).is_ok());
    config
        .features
        .set(requested)
        .expect("managed feature mutations should normalize successfully");

    assert!(config.features.enabled(Feature::Personality));
    assert!(!config.features.enabled(Feature::ShellTool));

    Ok(())
}

#[tokio::test]
async fn feature_requirements_reject_collab_legacy_alias() {
    let chaos_home = TempDir::new().expect("tempdir");

    let err = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .cloud_requirements(CloudRequirementsLoader::new(async {
            Ok(Some(crate::config_loader::ConfigRequirementsToml {
                feature_requirements: Some(crate::config_loader::FeatureRequirementsToml {
                    entries: BTreeMap::from([("collab".to_string(), true)]),
                }),
                ..Default::default()
            }))
        }))
        .build()
        .await
        .expect_err("legacy aliases should be rejected");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string()
            .contains("use canonical feature key `multi_agent`"),
        "{err}"
    );
}
