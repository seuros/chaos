use super::*;

#[test]
fn untrusted_project_never_resolves_wider_than_no_decision() -> anyhow::Result<()> {
    let cfg = ConfigToml::default();
    let resolve = |trust_level: Option<TrustLevel>| {
        cfg.derive_sandbox_policy(None, None, Some(&ProjectTrust { trust_level }), None)
    };

    let undecided = resolve(None);
    let denied = resolve(Some(TrustLevel::Untrusted));
    let granted = resolve(Some(TrustLevel::Trusted));

    // A denial is the same read-only default as having no decision; only a
    // grant widens to workspace-write.
    assert_eq!(undecided, SandboxPolicy::new_read_only_policy());
    assert_eq!(denied, undecided, "a denial must not widen the sandbox");
    std::assert_matches!(granted, SandboxPolicy::WorkspaceWrite { .. });

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
