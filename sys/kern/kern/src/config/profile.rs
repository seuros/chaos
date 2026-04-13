//! Re-export config profile from `chaos-config` and profile-resolution methods
//! for `ConfigToml`.

pub use chaos_sysctl::profile::*;

use std::path::Path;

use chaos_ipc::config_types::SandboxMode;
use chaos_sysctl::Constrained;

use crate::config::ConfigToml;
use crate::config::ProjectConfig;
use crate::git_info::resolve_root_git_project_for_trust;
use crate::protocol::ReadOnlyAccess;
use crate::protocol::SandboxPolicy;

impl ConfigToml {
    /// Derive the effective sandbox policy from the configuration.
    pub(crate) fn derive_sandbox_policy(
        &self,
        sandbox_mode_override: Option<SandboxMode>,
        profile_sandbox_mode: Option<SandboxMode>,
        resolved_cwd: &Path,
        sandbox_policy_constraint: Option<&Constrained<SandboxPolicy>>,
    ) -> SandboxPolicy {
        use crate::config::types::SandboxWorkspaceWrite;

        let sandbox_mode_was_explicit = sandbox_mode_override.is_some()
            || profile_sandbox_mode.is_some()
            || self.sandbox_mode.is_some();
        let resolved_sandbox_mode = sandbox_mode_override
            .or(profile_sandbox_mode)
            .or(self.sandbox_mode)
            .or_else(|| {
                self.get_active_project(resolved_cwd).and_then(|p| {
                    if p.is_trusted() || p.is_untrusted() {
                        Some(SandboxMode::WorkspaceWrite)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();
        let mut sandbox_policy = match resolved_sandbox_mode {
            SandboxMode::ReadOnly => SandboxPolicy::new_read_only_policy(),
            SandboxMode::WorkspaceWrite => match self.sandbox_workspace_write.as_ref() {
                Some(SandboxWorkspaceWrite {
                    writable_roots,
                    network_access,
                    exclude_tmpdir_env_var,
                    exclude_slash_tmp,
                }) => SandboxPolicy::WorkspaceWrite {
                    writable_roots: writable_roots.clone(),
                    read_only_access: ReadOnlyAccess::FullAccess,
                    network_access: *network_access,
                    exclude_tmpdir_env_var: *exclude_tmpdir_env_var,
                    exclude_slash_tmp: *exclude_slash_tmp,
                },
                None => SandboxPolicy::new_workspace_write_policy(),
            },
            SandboxMode::RootAccess => SandboxPolicy::RootAccess,
        };
        if !sandbox_mode_was_explicit
            && let Some(constraint) = sandbox_policy_constraint
            && let Err(err) = constraint.can_set(&sandbox_policy)
        {
            tracing::warn!(
                error = %err,
                "default sandbox policy is disallowed by requirements; falling back to required default"
            );
            sandbox_policy = constraint.get().clone();
        }
        sandbox_policy
    }

    /// Resolves the cwd to an existing project, or returns `None` when no
    /// matching project entry is found.
    pub fn get_active_project(&self, resolved_cwd: &Path) -> Option<ProjectConfig> {
        let projects = self.projects.clone().unwrap_or_default();

        if let Some(project_config) = projects.get(&resolved_cwd.to_string_lossy().to_string()) {
            return Some(project_config.clone());
        }

        if let Some(repo_root) = resolve_root_git_project_for_trust(resolved_cwd)
            && let Some(project_config_for_root) =
                projects.get(&repo_root.to_string_lossy().to_string())
        {
            return Some(project_config_for_root.clone());
        }

        None
    }

    pub fn get_config_profile(
        &self,
        override_profile: Option<String>,
    ) -> Result<ConfigProfile, std::io::Error> {
        let profile = override_profile.or_else(|| self.profile.clone());

        match profile {
            Some(key) => {
                if let Some(profile) = self.profiles.get(key.as_str()) {
                    return Ok(profile.clone());
                }

                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("config profile `{key}` not found"),
                ))
            }
            None => Ok(ConfigProfile::default()),
        }
    }
}
