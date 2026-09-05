/*
Module: sandboxing

Build platform wrappers and produce ExecRequest for execution. Owns low-level
sandbox placement and transformation of portable CommandSpec into a
ready‑to‑spawn environment.
*/

use crate::exec::ExecExpiration;
use crate::exec::ExecToolCallOutput;
use crate::exec::SandboxType;
use crate::exec::StdoutStream;
use crate::exec::execute_exec_request;
use crate::spawn::CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use crate::tools::sandboxing::SandboxablePreference;
use alcatraz::SandboxRequest as AlcatrazSandboxRequest;
use alcatraz::prepare_command as prepare_platform_command;
use chaos_ipc::models::FileSystemPermissions;
#[cfg(target_os = "macos")]
use chaos_ipc::models::MacOsSeatbeltProfileExtensions;
use chaos_ipc::models::NetworkPermissions;
use chaos_ipc::models::PermissionProfile;
pub use chaos_ipc::models::SandboxPermissions;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsAccessMode;
use chaos_ipc::permissions::VfsEntry;
use chaos_ipc::permissions::VfsPath;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::permissions::VfsPolicyKind;
use chaos_parole::macos_permissions::intersect_seatbelt_profile_extensions;
use chaos_parole::macos_permissions::merge_seatbelt_profile_extensions;
use chaos_parole::sandbox::has_full_disk_write_access;
use chaos_pf::NetworkProxy;
use chaos_realpath::AbsolutePathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::canonicalize;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub expiration: ExecExpiration,
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<PermissionProfile>,
    pub justification: Option<String>,
}

#[derive(Debug)]
pub struct ExecRequest {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub network: Option<NetworkProxy>,
    pub expiration: ExecExpiration,
    pub sandbox: SandboxType,
    pub sandbox_permissions: SandboxPermissions,
    pub vfs_policy: VfsPolicy,
    pub socket_policy: SocketPolicy,
    pub justification: Option<String>,
    pub arg0: Option<String>,
}

/// Bundled arguments for sandbox transformation.
///
/// This keeps call sites self-documenting when several fields are optional.
pub(crate) struct SandboxTransformRequest<'a> {
    pub spec: CommandSpec,
    pub file_system_policy: &'a VfsPolicy,
    pub network_policy: SocketPolicy,
    pub sandbox: SandboxType,
    pub enforce_managed_network: bool,
    // TODO(viyatb): Evaluate switching this to Option<Arc<NetworkProxy>>
    // to make shared ownership explicit across runtime/sandbox plumbing.
    pub network: Option<&'a NetworkProxy>,
    pub sandbox_policy_cwd: &'a Path,
    #[cfg(target_os = "macos")]
    pub macos_seatbelt_profile_extensions: Option<&'a MacOsSeatbeltProfileExtensions>,
    pub alcatraz_exe: &'a Path,
}

pub enum SandboxPreference {
    Auto,
    Require,
    Forbid,
}

#[derive(Debug, thiserror::Error)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum SandboxTransformError {
    #[error("failed to project split sandbox policies to a combined policy: {source}")]
    InvalidSandboxPolicyProjection { source: std::io::Error },
}

pub(crate) fn normalize_additional_permissions(
    additional_permissions: PermissionProfile,
) -> Result<PermissionProfile, String> {
    let network = additional_permissions
        .network
        .filter(|network| !network.is_empty());
    let file_system = additional_permissions
        .file_system
        .map(|file_system| {
            let read = file_system
                .read
                .map(|paths| normalize_permission_paths(paths, "file_system.read"));
            let write = file_system
                .write
                .map(|paths| normalize_permission_paths(paths, "file_system.write"));
            FileSystemPermissions { read, write }
        })
        .filter(|file_system| !file_system.is_empty());
    let macos = additional_permissions.macos;

    Ok(PermissionProfile {
        network,
        file_system,
        macos,
    })
}

pub(crate) fn merge_permission_profiles(
    base: Option<&PermissionProfile>,
    permissions: Option<&PermissionProfile>,
) -> Option<PermissionProfile> {
    let Some(permissions) = permissions else {
        return base.cloned();
    };

    match base {
        Some(base) => {
            let network = match (base.network.as_ref(), permissions.network.as_ref()) {
                (
                    Some(NetworkPermissions {
                        enabled: Some(true),
                    }),
                    _,
                )
                | (
                    _,
                    Some(NetworkPermissions {
                        enabled: Some(true),
                    }),
                ) => Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                _ => None,
            };
            let file_system = match (base.file_system.as_ref(), permissions.file_system.as_ref()) {
                (Some(base), Some(permissions)) => Some(FileSystemPermissions {
                    read: merge_permission_paths(base.read.as_ref(), permissions.read.as_ref()),
                    write: merge_permission_paths(base.write.as_ref(), permissions.write.as_ref()),
                })
                .filter(|file_system| !file_system.is_empty()),
                (Some(base), None) => Some(base.clone()),
                (None, Some(permissions)) => Some(permissions.clone()),
                (None, None) => None,
            };
            let macos =
                merge_seatbelt_profile_extensions(base.macos.as_ref(), permissions.macos.as_ref());

            Some(PermissionProfile {
                network,
                file_system,
                macos,
            })
            .filter(|permissions| !permissions.is_empty())
        }
        None => Some(permissions.clone()).filter(|permissions| !permissions.is_empty()),
    }
}

pub fn intersect_permission_profiles(
    requested: PermissionProfile,
    granted: PermissionProfile,
) -> PermissionProfile {
    let file_system = requested
        .file_system
        .map(|requested_file_system| {
            let granted_file_system = granted.file_system.unwrap_or_default();
            let read = requested_file_system
                .read
                .map(|requested_read| {
                    let granted_read = granted_file_system.read.unwrap_or_default();
                    requested_read
                        .into_iter()
                        .filter(|path| granted_read.contains(path))
                        .collect()
                })
                .filter(|paths: &Vec<_>| !paths.is_empty());
            let write = requested_file_system
                .write
                .map(|requested_write| {
                    let granted_write = granted_file_system.write.unwrap_or_default();
                    requested_write
                        .into_iter()
                        .filter(|path| granted_write.contains(path))
                        .collect()
                })
                .filter(|paths: &Vec<_>| !paths.is_empty());
            FileSystemPermissions { read, write }
        })
        .filter(|file_system| !file_system.is_empty());
    let network = match (requested.network, granted.network) {
        (
            Some(NetworkPermissions {
                enabled: Some(true),
            }),
            Some(NetworkPermissions {
                enabled: Some(true),
            }),
        ) => Some(NetworkPermissions {
            enabled: Some(true),
        }),
        _ => None,
    };

    let macos = intersect_seatbelt_profile_extensions(requested.macos, granted.macos);

    PermissionProfile {
        network,
        file_system,
        macos,
    }
}

fn normalize_permission_paths(
    paths: Vec<AbsolutePathBuf>,
    _permission_kind: &str,
) -> Vec<AbsolutePathBuf> {
    let mut out = Vec::with_capacity(paths.len());
    let mut seen = HashSet::new();

    for path in paths {
        let canonicalized = canonicalize(path.as_path())
            .ok()
            .and_then(|path| AbsolutePathBuf::from_absolute_path(path).ok())
            .unwrap_or(path);
        if seen.insert(canonicalized.clone()) {
            out.push(canonicalized);
        }
    }

    out
}

fn merge_permission_paths(
    base: Option<&Vec<AbsolutePathBuf>>,
    permissions: Option<&Vec<AbsolutePathBuf>>,
) -> Option<Vec<AbsolutePathBuf>> {
    match (base, permissions) {
        (Some(base), Some(permissions)) => {
            let mut merged = Vec::with_capacity(base.len() + permissions.len());
            let mut seen = HashSet::with_capacity(base.len() + permissions.len());

            for path in base.iter().chain(permissions.iter()) {
                if seen.insert(path.clone()) {
                    merged.push(path.clone());
                }
            }

            Some(merged).filter(|paths| !paths.is_empty())
        }
        (Some(base), None) => Some(base.clone()),
        (None, Some(permissions)) => Some(permissions.clone()),
        (None, None) => None,
    }
}

fn dedup_absolute_paths(paths: Vec<AbsolutePathBuf>) -> Vec<AbsolutePathBuf> {
    let mut out = Vec::with_capacity(paths.len());
    let mut seen = HashSet::new();
    for path in paths {
        if seen.insert(path.to_path_buf()) {
            out.push(path);
        }
    }
    out
}

fn additional_permission_roots(
    additional_permissions: &PermissionProfile,
) -> (Vec<AbsolutePathBuf>, Vec<AbsolutePathBuf>) {
    (
        dedup_absolute_paths(
            additional_permissions
                .file_system
                .as_ref()
                .and_then(|file_system| file_system.read.clone())
                .unwrap_or_default(),
        ),
        dedup_absolute_paths(
            additional_permissions
                .file_system
                .as_ref()
                .and_then(|file_system| file_system.write.clone())
                .unwrap_or_default(),
        ),
    )
}

fn merge_vfs_policy_with_additional_permissions(
    file_system_policy: &VfsPolicy,
    extra_reads: Vec<AbsolutePathBuf>,
    extra_writes: Vec<AbsolutePathBuf>,
) -> VfsPolicy {
    match file_system_policy.kind {
        VfsPolicyKind::Restricted => {
            let mut merged_policy = file_system_policy.clone();
            for path in extra_reads {
                let entry = VfsEntry {
                    path: VfsPath::Path { path },
                    access: VfsAccessMode::Read,
                };
                if !merged_policy.entries.contains(&entry) {
                    merged_policy.entries.push(entry);
                }
            }
            for path in extra_writes {
                let entry = VfsEntry {
                    path: VfsPath::Path { path },
                    access: VfsAccessMode::Write,
                };
                if !merged_policy.entries.contains(&entry) {
                    merged_policy.entries.push(entry);
                }
            }
            merged_policy
        }
        VfsPolicyKind::Unrestricted | VfsPolicyKind::ExternalSandbox => file_system_policy.clone(),
    }
}

pub(crate) fn effective_vfs_policy(
    file_system_policy: &VfsPolicy,
    additional_permissions: Option<&PermissionProfile>,
) -> VfsPolicy {
    let Some(additional_permissions) = additional_permissions else {
        return file_system_policy.clone();
    };

    let (extra_reads, extra_writes) = additional_permission_roots(additional_permissions);
    if extra_reads.is_empty() && extra_writes.is_empty() {
        file_system_policy.clone()
    } else {
        merge_vfs_policy_with_additional_permissions(file_system_policy, extra_reads, extra_writes)
    }
}

pub(crate) fn effective_socket_policy(
    network_policy: SocketPolicy,
    additional_permissions: Option<&PermissionProfile>,
) -> SocketPolicy {
    if additional_permissions
        .is_some_and(|permissions| merge_network_access(network_policy.is_enabled(), permissions))
    {
        SocketPolicy::Enabled
    } else {
        network_policy
    }
}

fn merge_network_access(
    base_network_access: bool,
    additional_permissions: &PermissionProfile,
) -> bool {
    base_network_access
        || additional_permissions
            .network
            .as_ref()
            .and_then(|network| network.enabled)
            .unwrap_or(false)
}

pub(crate) fn should_require_platform_sandbox(
    file_system_policy: &VfsPolicy,
    network_policy: SocketPolicy,
    has_managed_network_requirements: bool,
) -> bool {
    if has_managed_network_requirements {
        return true;
    }

    if !network_policy.is_enabled() {
        return !matches!(file_system_policy.kind, VfsPolicyKind::ExternalSandbox);
    }

    match file_system_policy.kind {
        VfsPolicyKind::Restricted => !has_full_disk_write_access(file_system_policy),
        VfsPolicyKind::Unrestricted | VfsPolicyKind::ExternalSandbox => false,
    }
}

#[derive(Default)]
pub struct SandboxManager;

impl SandboxManager {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn select_initial(
        &self,
        file_system_policy: &VfsPolicy,
        network_policy: SocketPolicy,
        pref: SandboxablePreference,
        has_managed_network_requirements: bool,
    ) -> SandboxType {
        // FreeBSD Capsicum: the Alcatraz helper applies what it can
        // (procctl hardening) and warns about unenforced dimensions. The
        // helper itself decides enforcement scope — the selector should not
        // reject it.
        match pref {
            SandboxablePreference::Forbid => SandboxType::None,
            SandboxablePreference::Require => crate::safety::get_platform_sandbox(),
            SandboxablePreference::Auto => {
                if should_require_platform_sandbox(
                    file_system_policy,
                    network_policy,
                    has_managed_network_requirements,
                ) {
                    crate::safety::get_platform_sandbox()
                } else {
                    SandboxType::None
                }
            }
        }
    }

    pub(crate) fn transform(
        &self,
        request: SandboxTransformRequest<'_>,
    ) -> Result<ExecRequest, SandboxTransformError> {
        let SandboxTransformRequest {
            mut spec,
            file_system_policy,
            network_policy,
            sandbox,
            enforce_managed_network,
            network,
            sandbox_policy_cwd,
            #[cfg(target_os = "macos")]
            macos_seatbelt_profile_extensions,
            alcatraz_exe,
        } = request;
        #[cfg(not(target_os = "macos"))]
        let macos_seatbelt_profile_extensions = None;
        let additional_permissions = spec.additional_permissions.take();
        let effective_macos_seatbelt_profile_extensions = merge_seatbelt_profile_extensions(
            macos_seatbelt_profile_extensions,
            additional_permissions
                .as_ref()
                .and_then(|permissions| permissions.macos.as_ref()),
        );
        let effective_platform_permissions =
            effective_macos_seatbelt_profile_extensions.map(|macos| PermissionProfile {
                network: None,
                file_system: None,
                macos: Some(macos),
            });
        let (effective_file_system_policy, effective_network_policy) =
            if let Some(additional_permissions) = additional_permissions {
                (
                    effective_vfs_policy(file_system_policy, Some(&additional_permissions)),
                    effective_socket_policy(network_policy, Some(&additional_permissions)),
                )
            } else {
                (file_system_policy.clone(), network_policy)
            };
        let mut env = spec.env;
        if !effective_network_policy.is_enabled() {
            env.insert(
                CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR.to_string(),
                "1".to_string(),
            );
        }

        let mut command = Vec::with_capacity(1 + spec.args.len());
        command.push(spec.program);
        command.append(&mut spec.args);

        let (command, sandbox_env, arg0_override) = if sandbox == SandboxType::None {
            (command, HashMap::new(), None)
        } else {
            let prepared = prepare_platform_command(AlcatrazSandboxRequest {
                executable: alcatraz_exe,
                command,
                file_system_policy: &effective_file_system_policy,
                network_policy: effective_network_policy,
                sandbox_policy_cwd,
                enforce_managed_network,
                network,
                platform_permissions: effective_platform_permissions.as_ref(),
            })
            .map_err(|source| SandboxTransformError::InvalidSandboxPolicyProjection { source })?;
            let mut command = Vec::with_capacity(1 + prepared.args.len());
            command.push(prepared.program.to_string_lossy().to_string());
            command.extend(prepared.args);
            (command, prepared.env, prepared.arg0)
        };

        env.extend(sandbox_env);

        Ok(ExecRequest {
            command,
            cwd: spec.cwd,
            env,
            network: network.cloned(),
            expiration: spec.expiration,
            sandbox,
            sandbox_permissions: spec.sandbox_permissions,
            vfs_policy: effective_file_system_policy,
            socket_policy: effective_network_policy,
            justification: spec.justification,
            arg0: arg0_override,
        })
    }

    pub fn denied(&self, sandbox: SandboxType, out: &ExecToolCallOutput) -> bool {
        crate::exec::is_likely_sandbox_denied(sandbox, out)
    }
}

pub async fn execute_env(
    exec_request: ExecRequest,
    stdout_stream: Option<StdoutStream>,
) -> crate::error::Result<ExecToolCallOutput> {
    execute_exec_request(exec_request, stdout_stream, /*after_spawn*/ None).await
}

pub async fn execute_exec_request_with_after_spawn(
    exec_request: ExecRequest,
    stdout_stream: Option<StdoutStream>,
    after_spawn: Option<Box<dyn FnOnce() + Send>>,
) -> crate::error::Result<ExecToolCallOutput> {
    execute_exec_request(exec_request, stdout_stream, after_spawn).await
}
