use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SocketPolicy;
use chaos_ipc::protocol::VfsPolicy;
use chaos_parole::sandbox::needs_direct_runtime_enforcement;
use chaos_parole::sandbox::sandbox_policies_match_semantics;
use chaos_parole::sandbox::vfs_policy_from_sandbox_policy;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct EffectiveSandboxPolicies {
    pub sandbox_policy: SandboxPolicy,
    pub vfs_policy: VfsPolicy,
    pub socket_policy: SocketPolicy,
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
    vfs_policy: Option<VfsPolicy>,
    socket_policy: Option<SocketPolicy>,
) -> Result<EffectiveSandboxPolicies, ResolveSandboxPoliciesError> {
    let split_policies = match (vfs_policy, socket_policy) {
        (Some(vfs_policy), Some(socket_policy)) => Some((vfs_policy, socket_policy)),
        (None, None) => None,
        _ => return Err(ResolveSandboxPoliciesError::PartialSplitPolicies),
    };

    match (sandbox_policy, split_policies) {
        (Some(sandbox_policy), Some((vfs_policy, socket_policy))) => {
            if needs_direct_runtime_enforcement(&vfs_policy, socket_policy, sandbox_policy_cwd) {
                return Ok(EffectiveSandboxPolicies {
                    sandbox_policy,
                    vfs_policy,
                    socket_policy,
                });
            }
            let derived_sandbox_policy = vfs_policy
                .to_sandbox_policy(socket_policy, sandbox_policy_cwd)
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
                vfs_policy,
                socket_policy,
            })
        }
        (Some(sandbox_policy), None) => Ok(EffectiveSandboxPolicies {
            vfs_policy: vfs_policy_from_sandbox_policy(&sandbox_policy, sandbox_policy_cwd),
            socket_policy: SocketPolicy::from(&sandbox_policy),
            sandbox_policy,
        }),
        (None, Some((vfs_policy, socket_policy))) => {
            let sandbox_policy = vfs_policy
                .to_sandbox_policy(socket_policy, sandbox_policy_cwd)
                .map_err(|err| {
                    ResolveSandboxPoliciesError::FailedToDeriveSandboxPolicy(err.to_string())
                })?;
            Ok(EffectiveSandboxPolicies {
                sandbox_policy,
                vfs_policy,
                socket_policy,
            })
        }
        (None, None) => Err(ResolveSandboxPoliciesError::MissingConfiguration),
    }
}
