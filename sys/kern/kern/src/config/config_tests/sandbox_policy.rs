use super::*;

#[test]
fn test_untrusted_project_gets_workspace_write_sandbox() -> anyhow::Result<()> {
    let cfg = ConfigToml::default();

    let resolution = cfg.derive_sandbox_policy(
        None,
        None,
        Some(&ProjectTrust {
            trust_level: Some(TrustLevel::Untrusted),
        }),
        None,
    );

    assert!(
        matches!(resolution, SandboxPolicy::WorkspaceWrite { .. }),
        "Expected WorkspaceWrite for untrusted project, got {resolution:?}"
    );

    Ok(())
}

#[test]
fn derive_sandbox_policy_falls_back_to_constraint_value_for_implicit_defaults() -> anyhow::Result<()>
{
    let cfg = ConfigToml::default();
    let constrained = Constrained::new(SandboxPolicy::RootAccess, |candidate| {
        if matches!(candidate, SandboxPolicy::RootAccess) {
            Ok(())
        } else {
            Err(ConstraintError::InvalidValue {
                field_name: "sandbox_mode",
                candidate: format!("{candidate:?}"),
                allowed: "[RootAccess]".to_string(),
                requirement_source: RequirementSource::Unknown,
            })
        }
    })?;

    let resolution = cfg.derive_sandbox_policy(
        None,
        None,
        Some(&ProjectTrust {
            trust_level: Some(TrustLevel::Trusted),
        }),
        Some(&constrained),
    );

    assert_eq!(resolution, SandboxPolicy::RootAccess);
    Ok(())
}

#[test]
fn derive_sandbox_policy_preserves_windows_downgrade_for_unsupported_fallback() -> anyhow::Result<()>
{
    let cfg = ConfigToml::default();
    let constrained = Constrained::new(SandboxPolicy::new_workspace_write_policy(), |candidate| {
        if matches!(candidate, SandboxPolicy::WorkspaceWrite { .. }) {
            Ok(())
        } else {
            Err(ConstraintError::InvalidValue {
                field_name: "sandbox_mode",
                candidate: format!("{candidate:?}"),
                allowed: "[WorkspaceWrite]".to_string(),
                requirement_source: RequirementSource::Unknown,
            })
        }
    })?;

    let resolution = cfg.derive_sandbox_policy(
        None,
        None,
        Some(&ProjectTrust {
            trust_level: Some(TrustLevel::Trusted),
        }),
        Some(&constrained),
    );

    assert_eq!(resolution, SandboxPolicy::new_workspace_write_policy());
    Ok(())
}
