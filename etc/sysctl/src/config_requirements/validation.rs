use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::protocol::SandboxPolicy;

use super::sourced::ConfigRequirementsWithSources;
use super::sourced::Sourced;
use super::types::AppsRequirementsToml;
use super::types::ConfigRequirements;
use super::types::ConstrainedWithSource;
use super::types::NetworkConstraints;
use super::types::SandboxModeRequirement;
use super::types::WebSearchModeRequirement;
use crate::Constrained;
use crate::ConstraintError;

/// Merge `enabled` configs from a lower-precedence source into an existing higher-precedence set.
/// This lets managed sources (for example Cloud/MDM) enforce setting disablement across layers.
/// Implemented with AppsRequirementsToml for now, could be abstracted if we have more enablement-style configs in the future.
pub(crate) fn merge_enablement_settings_descending(
    base: &mut AppsRequirementsToml,
    incoming: AppsRequirementsToml,
) {
    for (app_id, incoming_requirement) in incoming.apps {
        let base_requirement = base.apps.entry(app_id).or_default();
        let higher_precedence = base_requirement.enabled;
        let lower_precedence = incoming_requirement.enabled;
        base_requirement.enabled =
            if higher_precedence == Some(false) || lower_precedence == Some(false) {
                Some(false)
            } else {
                higher_precedence.or(lower_precedence)
            };
    }
}

impl TryFrom<ConfigRequirementsWithSources> for ConfigRequirements {
    type Error = ConstraintError;

    fn try_from(toml: ConfigRequirementsWithSources) -> Result<Self, Self::Error> {
        let ConfigRequirementsWithSources {
            allowed_approval_policies,
            allowed_sandbox_modes,
            allowed_web_search_modes,
            mcp_servers,
            apps: _apps,
            rules,
            enforce_residency,
            network,
        } = toml;

        let approval_policy = match allowed_approval_policies {
            Some(Sourced {
                value: policies,
                source: requirement_source,
            }) => {
                let Some(initial_value) = policies.first().copied() else {
                    return Err(ConstraintError::empty_field("allowed_approval_policies"));
                };

                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(initial_value, move |candidate| {
                    if policies.contains(candidate) {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "approval_policy",
                            candidate: format!("{candidate:?}"),
                            allowed: format!("{policies:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any_from_default(),
                /*source*/ None,
            ),
        };

        // TODO(gt): `ConfigRequirementsToml` should let the author specify the
        // default `SandboxPolicy`? Should do this for `ApprovalPolicy` too?
        //
        // Currently, we force ReadOnly as the default policy because two of
        // the other variants (WorkspaceWrite, ExternalSandbox) require
        // additional parameters. Ultimately, we should expand the config
        // format to allow specifying those parameters.
        let default_sandbox_policy = SandboxPolicy::new_read_only_policy();
        let sandbox_policy = match allowed_sandbox_modes {
            Some(Sourced {
                value: modes,
                source: requirement_source,
            }) => {
                if !modes.contains(&SandboxModeRequirement::ReadOnly) {
                    return Err(ConstraintError::InvalidValue {
                        field_name: "allowed_sandbox_modes",
                        candidate: format!("{modes:?}"),
                        allowed: "must include 'read-only' to allow any SandboxPolicy".to_string(),
                        requirement_source,
                    });
                };

                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(default_sandbox_policy, move |candidate| {
                    let mode = match candidate {
                        SandboxPolicy::ReadOnly { .. } => SandboxModeRequirement::ReadOnly,
                        SandboxPolicy::WorkspaceWrite { .. } => {
                            SandboxModeRequirement::WorkspaceWrite
                        }
                        SandboxPolicy::RootAccess => SandboxModeRequirement::RootAccess,
                        SandboxPolicy::ExternalSandbox { .. } => {
                            SandboxModeRequirement::ExternalSandbox
                        }
                    };
                    if modes.contains(&mode) {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "sandbox_mode",
                            candidate: format!("{mode:?}"),
                            allowed: format!("{modes:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => {
                ConstrainedWithSource::new(
                    Constrained::allow_any(default_sandbox_policy),
                    /*source*/ None,
                )
            }
        };
        let exec_policy = match rules {
            Some(Sourced { value, source }) => {
                let policy = value.to_requirements_policy().map_err(|err| {
                    ConstraintError::ExecPolicyParse {
                        requirement_source: source.clone(),
                        reason: err.to_string(),
                    }
                })?;
                Some(Sourced::new(policy, source))
            }
            None => None,
        };
        let web_search_mode = match allowed_web_search_modes {
            Some(Sourced {
                value: modes,
                source: requirement_source,
            }) => {
                let mut accepted = modes.into_iter().collect::<std::collections::BTreeSet<_>>();
                accepted.insert(WebSearchModeRequirement::Disabled);
                let allowed_for_error = format!(
                    "{:?}",
                    accepted
                        .iter()
                        .copied()
                        .map(WebSearchMode::from)
                        .collect::<Vec<_>>()
                );

                let initial_value = if accepted.contains(&WebSearchModeRequirement::Cached) {
                    WebSearchMode::Cached
                } else if accepted.contains(&WebSearchModeRequirement::Live) {
                    WebSearchMode::Live
                } else {
                    WebSearchMode::Disabled
                };
                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(initial_value, move |candidate| {
                    if accepted.contains(&(*candidate).into()) {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "web_search_mode",
                            candidate: format!("{candidate:?}"),
                            allowed: allowed_for_error.clone(),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any(WebSearchMode::Cached),
                /*source*/ None,
            ),
        };
        let enforce_residency = match enforce_residency {
            Some(Sourced {
                value: residency,
                source: requirement_source,
            }) => {
                let required = Some(residency);
                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(required, move |candidate| {
                    if candidate == &required {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "enforce_residency",
                            candidate: format!("{candidate:?}"),
                            allowed: format!("{required:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any(/*initial_value*/ None),
                /*source*/ None,
            ),
        };
        let network = network.map(|sourced_network| {
            let Sourced { value, source } = sourced_network;
            Sourced::new(NetworkConstraints::from(value), source)
        });
        Ok(ConfigRequirements {
            approval_policy,
            sandbox_policy,
            web_search_mode,
            mcp_servers,
            exec_policy,
            enforce_residency,
            network,
        })
    }
}

impl From<super::types::NetworkRequirementsToml> for NetworkConstraints {
    fn from(value: super::types::NetworkRequirementsToml) -> Self {
        let super::types::NetworkRequirementsToml {
            enabled,
            http_port,
            socks_port,
            allow_upstream_proxy,
            dangerously_allow_non_loopback_proxy,
            dangerously_allow_all_unix_sockets,
            allowed_domains,
            managed_allowed_domains_only,
            denied_domains,
            allow_unix_sockets,
            allow_local_binding,
        } = value;
        Self {
            enabled,
            http_port,
            socks_port,
            allow_upstream_proxy,
            dangerously_allow_non_loopback_proxy,
            dangerously_allow_all_unix_sockets,
            allowed_domains,
            managed_allowed_domains_only,
            denied_domains,
            allow_unix_sockets,
            allow_local_binding,
        }
    }
}

#[cfg(test)]
mod tests;
