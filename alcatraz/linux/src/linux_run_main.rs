use clap::Parser;
use std::ffi::CString;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;

use crate::landlock::apply_sandbox_policy_to_current_thread;
use codex_protocol::protocol::FileSystemSandboxPolicy;
use codex_protocol::protocol::NetworkSandboxPolicy;
use codex_protocol::protocol::SandboxPolicy;

#[derive(Debug, Parser)]
/// CLI surface for the Linux sandbox helper.
///
/// Applies landlock filesystem restrictions and seccomp syscall filters
/// in-process, then execs the target command.
pub struct LandlockCommand {
    /// It is possible that the cwd used in the context of the sandbox policy
    /// is different from the cwd of the process to spawn.
    #[arg(long = "sandbox-policy-cwd")]
    pub sandbox_policy_cwd: PathBuf,

    /// Legacy compatibility policy.
    ///
    /// Newer callers pass split filesystem/network policies as well so the
    /// helper can migrate incrementally without breaking older invocations.
    #[arg(long = "sandbox-policy", hide = true)]
    pub sandbox_policy: Option<SandboxPolicy>,

    #[arg(long = "file-system-sandbox-policy", hide = true)]
    pub file_system_sandbox_policy: Option<FileSystemSandboxPolicy>,

    #[arg(long = "network-sandbox-policy", hide = true)]
    pub network_sandbox_policy: Option<NetworkSandboxPolicy>,

    /// Internal compatibility flag.
    ///
    /// By default, restricted-network sandboxing uses isolated networking.
    /// If set, sandbox setup switches to proxy-only network mode.
    #[arg(long = "allow-network-for-proxy", hide = true, default_value_t = false)]
    pub allow_network_for_proxy: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "use-legacy-landlock", hide = true, default_value_t = false)]
    pub _use_legacy_landlock: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "apply-seccomp-then-exec", hide = true, default_value_t = false)]
    pub _apply_seccomp_then_exec: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "no-proc", hide = true, default_value_t = false)]
    pub _no_proc: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "proxy-route-spec", hide = true)]
    pub _proxy_route_spec: Option<String>,

    /// Full command args to run under the Linux sandbox helper.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

/// Entry point for the Linux sandbox helper.
///
/// 1. Apply landlock filesystem restrictions + seccomp syscall filters in-process.
/// 2. `execvp` into the final command.
pub fn run_main() -> ! {
    let LandlockCommand {
        sandbox_policy_cwd,
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
        allow_network_for_proxy,
        command,
        ..
    } = LandlockCommand::parse();

    if command.is_empty() {
        panic!("No command specified to execute.");
    }

    let EffectiveSandboxPolicies {
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
    } = resolve_sandbox_policies(
        sandbox_policy_cwd.as_path(),
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
    )
    .unwrap_or_else(|err| panic!("{err}"));

    let apply_landlock_fs = !file_system_sandbox_policy.has_full_disk_write_access()
        || allow_network_for_proxy;

    if let Err(e) = apply_sandbox_policy_to_current_thread(
        &sandbox_policy,
        network_sandbox_policy,
        &sandbox_policy_cwd,
        apply_landlock_fs,
        allow_network_for_proxy,
        /*proxy_routed_network*/ false,
    ) {
        panic!("error applying Linux sandbox restrictions: {e:?}");
    }

    exec_or_panic(command);
}

#[derive(Debug, Clone)]
struct EffectiveSandboxPolicies {
    sandbox_policy: SandboxPolicy,
    file_system_sandbox_policy: FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
}

#[derive(Debug, PartialEq, Eq)]
enum ResolveSandboxPoliciesError {
    PartialSplitPolicies,
    SplitPoliciesRequireDirectRuntimeEnforcement(String),
    FailedToDeriveLegacyPolicy(String),
    MismatchedLegacyPolicy {
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
                    "split sandbox policies require direct runtime enforcement and cannot be paired with legacy sandbox policy: {err}"
                )
            }
            Self::FailedToDeriveLegacyPolicy(err) => {
                write!(
                    f,
                    "failed to derive legacy sandbox policy from split policies: {err}"
                )
            }
            Self::MismatchedLegacyPolicy { provided, derived } => {
                write!(
                    f,
                    "legacy sandbox policy must match split sandbox policies: provided={provided:?}, derived={derived:?}"
                )
            }
            Self::MissingConfiguration => write!(f, "missing sandbox policy configuration"),
        }
    }
}

fn resolve_sandbox_policies(
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
            if file_system_sandbox_policy
                .needs_direct_runtime_enforcement(network_sandbox_policy, sandbox_policy_cwd)
            {
                return Ok(EffectiveSandboxPolicies {
                    sandbox_policy,
                    file_system_sandbox_policy,
                    network_sandbox_policy,
                });
            }
            let derived_legacy_policy = file_system_sandbox_policy
                .to_legacy_sandbox_policy(network_sandbox_policy, sandbox_policy_cwd)
                .map_err(|err| {
                    ResolveSandboxPoliciesError::SplitPoliciesRequireDirectRuntimeEnforcement(
                        err.to_string(),
                    )
                })?;
            if !legacy_sandbox_policies_match_semantics(
                &sandbox_policy,
                &derived_legacy_policy,
                sandbox_policy_cwd,
            ) {
                return Err(ResolveSandboxPoliciesError::MismatchedLegacyPolicy {
                    provided: sandbox_policy,
                    derived: derived_legacy_policy,
                });
            }
            Ok(EffectiveSandboxPolicies {
                sandbox_policy,
                file_system_sandbox_policy,
                network_sandbox_policy,
            })
        }
        (Some(sandbox_policy), None) => Ok(EffectiveSandboxPolicies {
            file_system_sandbox_policy: FileSystemSandboxPolicy::from_legacy_sandbox_policy(
                &sandbox_policy,
                sandbox_policy_cwd,
            ),
            network_sandbox_policy: NetworkSandboxPolicy::from(&sandbox_policy),
            sandbox_policy,
        }),
        (None, Some((file_system_sandbox_policy, network_sandbox_policy))) => {
            let sandbox_policy = file_system_sandbox_policy
                .to_legacy_sandbox_policy(network_sandbox_policy, sandbox_policy_cwd)
                .map_err(|err| {
                    ResolveSandboxPoliciesError::FailedToDeriveLegacyPolicy(err.to_string())
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

fn legacy_sandbox_policies_match_semantics(
    provided: &SandboxPolicy,
    derived: &SandboxPolicy,
    sandbox_policy_cwd: &Path,
) -> bool {
    NetworkSandboxPolicy::from(provided) == NetworkSandboxPolicy::from(derived)
        && file_system_sandbox_policies_match_semantics(
            &FileSystemSandboxPolicy::from_legacy_sandbox_policy(provided, sandbox_policy_cwd),
            &FileSystemSandboxPolicy::from_legacy_sandbox_policy(derived, sandbox_policy_cwd),
            sandbox_policy_cwd,
        )
}

fn file_system_sandbox_policies_match_semantics(
    provided: &FileSystemSandboxPolicy,
    derived: &FileSystemSandboxPolicy,
    sandbox_policy_cwd: &Path,
) -> bool {
    provided.has_full_disk_read_access() == derived.has_full_disk_read_access()
        && provided.has_full_disk_write_access() == derived.has_full_disk_write_access()
        && provided.include_platform_defaults() == derived.include_platform_defaults()
        && provided.get_readable_roots_with_cwd(sandbox_policy_cwd)
            == derived.get_readable_roots_with_cwd(sandbox_policy_cwd)
        && provided.get_writable_roots_with_cwd(sandbox_policy_cwd)
            == derived.get_writable_roots_with_cwd(sandbox_policy_cwd)
        && provided.get_unreadable_roots_with_cwd(sandbox_policy_cwd)
            == derived.get_unreadable_roots_with_cwd(sandbox_policy_cwd)
}

/// Exec the provided argv, panicking with context if it fails.
fn exec_or_panic(command: Vec<String>) -> ! {
    #[expect(clippy::expect_used)]
    let c_command =
        CString::new(command[0].as_str()).expect("Failed to convert command to CString");
    #[expect(clippy::expect_used)]
    let c_args: Vec<CString> = command
        .iter()
        .map(|arg| CString::new(arg.as_str()).expect("Failed to convert arg to CString"))
        .collect();

    let mut c_args_ptrs: Vec<*const libc::c_char> = c_args.iter().map(|arg| arg.as_ptr()).collect();
    c_args_ptrs.push(std::ptr::null());

    unsafe {
        libc::execvp(c_command.as_ptr(), c_args_ptrs.as_ptr());
    }

    // If execvp returns, there was an error.
    let err = std::io::Error::last_os_error();
    panic!("Failed to execvp {}: {err}", command[0].as_str());
}

#[cfg(test)]
#[path = "linux_run_main_tests.rs"]
mod tests;
