use clap::Parser;
use std::ffi::CString;
use std::ffi::OsStr;
use std::fmt;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;

use crate::capsicum::apply_sandbox_policy_to_current_thread;
use chaos_ipc::protocol::FileSystemSandboxPolicy;
use chaos_ipc::protocol::NetworkSandboxPolicy;
use chaos_ipc::protocol::SandboxPolicy;

#[derive(Debug, Parser)]
/// CLI surface for the FreeBSD sandbox helper.
///
/// Validates which sandbox policy combinations can be enforced safely on
/// FreeBSD, then execs the target command. Mirrors the alcatraz-linux
/// interface for consistent cross-platform sandbox invocation.
pub struct CapsicumCommand {
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
    /// Managed-network sessions request proxy-only connectivity through this
    /// flag. The current FreeBSD helper rejects that mode explicitly because
    /// proxy-routed networking is not implemented yet.
    #[arg(long = "allow-network-for-proxy", hide = true, default_value_t = false)]
    pub allow_network_for_proxy: bool,

    /// Full command args to run under the FreeBSD sandbox helper.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

/// Entry point for the FreeBSD sandbox helper.
///
/// 1. Resolve the executable before any restrictive sandbox setup.
/// 2. Apply supported Capsicum restrictions in-process.
/// 3. `fexecve` into the final command.
pub fn run_main() -> ! {
    let CapsicumCommand {
        sandbox_policy_cwd,
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
        allow_network_for_proxy,
        command,
    } = CapsicumCommand::parse();

    if command.is_empty() {
        panic!("No command specified to execute.");
    }

    let EffectiveSandboxPolicies {
        sandbox_policy: _sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
    } = resolve_sandbox_policies(
        sandbox_policy_cwd.as_path(),
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
    )
    .unwrap_or_else(|err| panic!("{err}"));

    let prepared_exec = prepare_exec(command);

    if let Err(e) = apply_sandbox_policy_to_current_thread(
        &file_system_sandbox_policy,
        network_sandbox_policy,
        allow_network_for_proxy,
        /*proxy_routed_network*/ allow_network_for_proxy,
    ) {
        panic!("error applying FreeBSD sandbox restrictions: {e:?}");
    }

    prepared_exec.exec_or_panic();
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

struct PreparedExec {
    executable_fd: OwnedFd,
    executable_label: String,
    command: Vec<String>,
}

impl PreparedExec {
    fn exec_or_panic(self) -> ! {
        let c_args: Vec<CString> = self
            .command
            .iter()
            .map(|arg| c_string_from_str(arg.as_str(), "command arg"))
            .collect();
        let c_env: Vec<CString> = collect_envp();

        let mut c_args_ptrs: Vec<*const libc::c_char> =
            c_args.iter().map(|arg| arg.as_ptr()).collect();
        c_args_ptrs.push(std::ptr::null());

        let mut c_env_ptrs: Vec<*const libc::c_char> =
            c_env.iter().map(|entry| entry.as_ptr()).collect();
        c_env_ptrs.push(std::ptr::null());

        unsafe {
            libc::fexecve(
                self.executable_fd.as_raw_fd(),
                c_args_ptrs.as_ptr(),
                c_env_ptrs.as_ptr(),
            );
        }

        let err = std::io::Error::last_os_error();
        panic!("Failed to fexecve {}: {err}", self.executable_label);
    }
}

fn prepare_exec(command: Vec<String>) -> PreparedExec {
    let executable_label = command[0].clone();
    let executable_fd = resolve_executable_fd(executable_label.as_str())
        .unwrap_or_else(|err| panic!("Failed to resolve executable {}: {err}", executable_label));

    PreparedExec {
        executable_fd,
        executable_label,
        command,
    }
}

fn resolve_executable_fd(program: &str) -> std::io::Result<OwnedFd> {
    let path = std::env::var_os("PATH").or_else(default_exec_path);
    resolve_executable_fd_with_search_path(program, path.as_deref())
}

fn resolve_executable_fd_with_search_path(
    program: &str,
    search_path: Option<&OsStr>,
) -> std::io::Result<OwnedFd> {
    if program.contains('/') {
        return open_executable_fd(Path::new(program));
    }

    let search_path = search_path.unwrap_or_else(|| OsStr::new("/bin:/usr/bin"));
    let mut permission_error = None;

    for dir in std::env::split_paths(search_path) {
        let candidate = if dir.as_os_str().is_empty() {
            PathBuf::from(program)
        } else {
            dir.join(program)
        };

        match open_executable_fd(candidate.as_path()) {
            Ok(fd) => return Ok(fd),
            Err(err) => match err.raw_os_error() {
                Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EISDIR) => {
                    if permission_error.is_none() {
                        permission_error = Some(err);
                    }
                }
                Some(libc::ENOENT) | Some(libc::ENOTDIR) => {}
                _ => return Err(err),
            },
        }
    }

    Err(permission_error.unwrap_or_else(|| std::io::Error::from_raw_os_error(libc::ENOENT)))
}

fn default_exec_path() -> Option<std::ffi::OsString> {
    let len = unsafe { libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0) };
    if len == 0 {
        return None;
    }

    let mut buffer = vec![0_u8; len];
    let written =
        unsafe { libc::confstr(libc::_CS_PATH, buffer.as_mut_ptr().cast(), buffer.len()) };
    if written == 0 {
        return None;
    }

    if buffer.last() == Some(&0) {
        let _ = buffer.pop();
    }

    Some(std::ffi::OsString::from_vec(buffer))
}

fn open_executable_fd(path: &Path) -> std::io::Result<OwnedFd> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path contains interior NUL bytes: {}", path.display()),
        )
    })?;

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_EXEC | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn collect_envp() -> Vec<CString> {
    std::env::vars_os()
        .map(|(key, value)| {
            let mut entry = key.into_vec();
            entry.push(b'=');
            entry.extend(value.into_vec());
            c_string_from_bytes(entry, "environment entry")
        })
        .collect()
}

fn c_string_from_str(value: &str, field: &str) -> CString {
    match CString::new(value) {
        Ok(value) => value,
        Err(_) => panic!("{field} contains interior NUL bytes"),
    }
}

fn c_string_from_bytes(value: Vec<u8>, field: &str) -> CString {
    match CString::new(value) {
        Ok(value) => value,
        Err(_) => panic!("{field} contains interior NUL bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::protocol::FileSystemSandboxPolicy;
    use chaos_ipc::protocol::NetworkSandboxPolicy;
    use chaos_ipc::protocol::SandboxPolicy;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn resolve_sandbox_policies_derives_split_policies_from_legacy_policy() {
        let sandbox_policy = SandboxPolicy::new_read_only_policy();

        let resolved =
            resolve_sandbox_policies(Path::new("/tmp"), Some(sandbox_policy.clone()), None, None)
                .expect("legacy policy should resolve");

        assert_eq!(resolved.sandbox_policy, sandbox_policy);
        assert_eq!(
            resolved.file_system_sandbox_policy,
            FileSystemSandboxPolicy::from(&sandbox_policy)
        );
        assert_eq!(
            resolved.network_sandbox_policy,
            NetworkSandboxPolicy::from(&sandbox_policy)
        );
    }

    #[test]
    fn resolve_sandbox_policies_derives_legacy_policy_from_split_policies() {
        let sandbox_policy = SandboxPolicy::new_read_only_policy();
        let file_system_sandbox_policy = FileSystemSandboxPolicy::from(&sandbox_policy);
        let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);

        let resolved = resolve_sandbox_policies(
            Path::new("/tmp"),
            None,
            Some(file_system_sandbox_policy.clone()),
            Some(network_sandbox_policy),
        )
        .expect("split policies should resolve");

        assert_eq!(resolved.sandbox_policy, sandbox_policy);
        assert_eq!(
            resolved.file_system_sandbox_policy,
            file_system_sandbox_policy
        );
        assert_eq!(resolved.network_sandbox_policy, network_sandbox_policy);
    }

    #[test]
    fn resolve_sandbox_policies_rejects_partial_split_policies() {
        let err = resolve_sandbox_policies(
            Path::new("/tmp"),
            Some(SandboxPolicy::new_read_only_policy()),
            Some(FileSystemSandboxPolicy::default()),
            None,
        )
        .expect_err("partial split policies should fail");

        assert_eq!(err, ResolveSandboxPoliciesError::PartialSplitPolicies);
    }

    #[test]
    fn resolve_sandbox_policies_rejects_mismatched_legacy_and_split_inputs() {
        let err = resolve_sandbox_policies(
            Path::new("/tmp"),
            Some(SandboxPolicy::new_read_only_policy()),
            Some(FileSystemSandboxPolicy::unrestricted()),
            Some(NetworkSandboxPolicy::Enabled),
        )
        .expect_err("mismatched legacy and split policies should fail");
        assert!(
            matches!(
                err,
                ResolveSandboxPoliciesError::MismatchedLegacyPolicy { .. }
            ),
            "{err}"
        );
    }

    #[test]
    fn resolve_executable_fd_uses_supplied_search_path() {
        let tempdir = TempDir::new().unwrap_or_else(|err| panic!("tempdir: {err}"));
        let program = tempdir.path().join("hello");
        std::fs::write(&program, b"#!/bin/sh\nexit 0\n")
            .unwrap_or_else(|err| panic!("write executable: {err}"));
        let mut permissions = std::fs::metadata(&program)
            .unwrap_or_else(|err| panic!("metadata: {err}"))
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions)
            .unwrap_or_else(|err| panic!("chmod executable: {err}"));

        let fd = resolve_executable_fd_with_search_path("hello", Some(tempdir.path().as_os_str()))
            .unwrap_or_else(|err| panic!("resolve executable: {err}"));

        drop(fd);
    }
}
