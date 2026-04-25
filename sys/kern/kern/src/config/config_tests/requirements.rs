use super::*;

#[test]
fn test_untrusted_project_gets_unless_trusted_approval_policy() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    let test_project_dir = TempDir::new()?;
    let test_path = test_project_dir.path();

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml::default(),
        ConfigOverrides {
            cwd: Some(test_path.to_path_buf()),
            active_project_trust: Some(ProjectTrust {
                trust_level: Some(TrustLevel::Untrusted),
            }),
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
