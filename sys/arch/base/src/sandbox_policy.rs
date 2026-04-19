use chaos_ipc::protocol::FileSystemSandboxPolicy;
use chaos_ipc::protocol::NetworkSandboxPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_parole::sandbox::file_system_policy_from_sandbox_policy;
use chaos_parole::sandbox::needs_direct_runtime_enforcement;
use chaos_parole::sandbox::sandbox_policies_match_semantics;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct EffectiveSandboxPolicies {
    pub sandbox_policy: SandboxPolicy,
    pub file_system_sandbox_policy: FileSystemSandboxPolicy,
    pub network_sandbox_policy: NetworkSandboxPolicy,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResolveSandboxPoliciesError {
    PartialSplitPolicies,
    SplitPoliciesRequireDirectRuntimeEnforcement(String),
    FailedToDeriveSandboxPolicy(String),
    MismatchedSandboxPolicy {
        provided: SandboxPolicy,
        derived: SandboxPolicy,
    },
    MissingConfiguration,
}

impl fmt::Display for ResolveSandboxPoliciesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PartialSplitPolicies => {
                write!(
                    f,
                    "file-system and network sandbox policies must be provided together"
                )
            }
            Self::SplitPoliciesRequireDirectRuntimeEnforcement(err) => {
                write!(
                    f,
                    "split sandbox policies require direct runtime enforcement and cannot be paired with a mismatched sandbox policy: {err}"
                )
            }
            Self::FailedToDeriveSandboxPolicy(err) => {
                write!(
                    f,
                    "failed to derive sandbox policy from split policies: {err}"
                )
            }
            Self::MismatchedSandboxPolicy { provided, derived } => {
                write!(
                    f,
                    "sandbox policy must match split sandbox policies: provided={provided:?}, derived={derived:?}"
                )
            }
            Self::MissingConfiguration => write!(f, "missing sandbox policy configuration"),
        }
    }
}

pub fn resolve_sandbox_policies(
    sandbox_policy_cwd: &Path,
    sandbox_policy: Option<SandboxPolicy>,
    file_system_sandbox_policy: Option<FileSystemSandboxPolicy>,
    network_sandbox_policy: Option<NetworkSandboxPolicy>,
) -> Result<EffectiveSandboxPolicies, ResolveSandboxPoliciesError> {
    let split_policies = match (file_system_sandbox_policy, network_sandbox_policy) {
        (Some(file_system_sandbox_policy), Some(network_sandbox_policy)) => {
            Some((file_system_sandbox_policy, network_sandbox_policy))
        }
        (None, None) => None,
        _ => return Err(ResolveSandboxPoliciesError::PartialSplitPolicies),
    };

    match (sandbox_policy, split_policies) {
        (Some(sandbox_policy), Some((file_system_sandbox_policy, network_sandbox_policy))) => {
            if needs_direct_runtime_enforcement(
                &file_system_sandbox_policy,
                network_sandbox_policy,
                sandbox_policy_cwd,
            ) {
                return Ok(EffectiveSandboxPolicies {
                    sandbox_policy,
                    file_system_sandbox_policy,
                    network_sandbox_policy,
                });
            }
            let derived_sandbox_policy = file_system_sandbox_policy
                .to_sandbox_policy(network_sandbox_policy, sandbox_policy_cwd)
                .map_err(|err| {
                    ResolveSandboxPoliciesError::SplitPoliciesRequireDirectRuntimeEnforcement(
                        err.to_string(),
                    )
                })?;
            if !sandbox_policies_match_semantics(
                &sandbox_policy,
                &derived_sandbox_policy,
                sandbox_policy_cwd,
            ) {
                return Err(ResolveSandboxPoliciesError::MismatchedSandboxPolicy {
                    provided: sandbox_policy,
                    derived: derived_sandbox_policy,
                });
            }
            Ok(EffectiveSandboxPolicies {
                sandbox_policy,
                file_system_sandbox_policy,
                network_sandbox_policy,
            })
        }
        (Some(sandbox_policy), None) => Ok(EffectiveSandboxPolicies {
            file_system_sandbox_policy: file_system_policy_from_sandbox_policy(
                &sandbox_policy,
                sandbox_policy_cwd,
            ),
            network_sandbox_policy: NetworkSandboxPolicy::from(&sandbox_policy),
            sandbox_policy,
        }),
        (None, Some((file_system_sandbox_policy, network_sandbox_policy))) => {
            let sandbox_policy = file_system_sandbox_policy
                .to_sandbox_policy(network_sandbox_policy, sandbox_policy_cwd)
                .map_err(|err| {
                    ResolveSandboxPoliciesError::FailedToDeriveSandboxPolicy(err.to_string())
                })?;
            Ok(EffectiveSandboxPolicies {
                sandbox_policy,
                file_system_sandbox_policy,
                network_sandbox_policy,
            })
        }
        (None, None) => Err(ResolveSandboxPoliciesError::MissingConfiguration),
    }
}
