use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

use chaos_realpath::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use tracing::error;
use ts_rs::TS;

/// Determines the conditions under which the user is consulted to approve
/// running the command proposed by Chaos.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    JsonSchema,
    TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ApprovalPolicy {
    /// Under this policy, only "known safe" commands—as determined by
    /// `is_safe_command()`—that **only read files** are auto‑approved.
    /// Everything else will ask the user to approve.
    Supervised,

    /// The model decides when to ask the user for approval.
    #[default]
    Interactive,

    /// Fine-grained controls for individual approval flows.
    ///
    /// When a field is `true`, commands in that category are allowed. When it
    /// is `false`, those requests are automatically rejected instead of shown
    /// to the user.
    #[strum(serialize = "granular")]
    Granular(GranularApprovalConfig),

    /// Never ask the user to approve commands. Failures are immediately returned
    /// to the model, and never escalated to the user for approval.
    Headless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct GranularApprovalConfig {
    /// Whether to allow shell command approval requests, including inline
    /// `with_additional_permissions` and `require_escalated` requests.
    pub sandbox_approval: bool,
    /// Whether to allow prompts triggered by execpolicy `prompt` rules.
    pub rules: bool,
    /// Whether to allow approval prompts triggered by skill script execution.
    #[serde(default)]
    pub skill_approval: bool,
    /// Whether to allow prompts triggered by the `request_permissions` tool.
    #[serde(default)]
    pub request_permissions: bool,
    /// Whether to allow MCP elicitation prompts.
    pub mcp_elicitations: bool,
}

impl GranularApprovalConfig {
    pub const fn allows_sandbox_approval(self) -> bool {
        self.sandbox_approval
    }

    pub const fn allows_rules_approval(self) -> bool {
        self.rules
    }

    pub const fn allows_skill_approval(self) -> bool {
        self.skill_approval
    }

    pub const fn allows_request_permissions(self) -> bool {
        self.request_permissions
    }

    pub const fn allows_mcp_elicitations(self) -> bool {
        self.mcp_elicitations
    }
}

/// Represents whether outbound network access is available to the agent.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, Default, JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum NetworkAccess {
    #[default]
    Restricted,
    Enabled,
}

impl NetworkAccess {
    pub fn is_enabled(self) -> bool {
        matches!(self, NetworkAccess::Enabled)
    }
}

fn default_include_platform_defaults() -> bool {
    true
}

/// Determines how read-only file access is granted inside a restricted
/// sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Display, Default, JsonSchema, TS)]
#[strum(serialize_all = "kebab-case")]
#[serde(tag = "type", rename_all = "kebab-case")]
#[ts(tag = "type")]
pub enum ReadOnlyAccess {
    /// Restrict reads to an explicit set of roots.
    ///
    /// When `include_platform_defaults` is `true`, platform defaults required
    /// for basic execution are included in addition to `readable_roots`.
    Restricted {
        /// Include built-in platform read roots required for basic process
        /// execution.
        #[serde(default = "default_include_platform_defaults")]
        include_platform_defaults: bool,
        /// Additional absolute roots that should be readable.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        readable_roots: Vec<AbsolutePathBuf>,
    },

    /// Allow unrestricted file reads.
    #[default]
    FullAccess,
}

impl ReadOnlyAccess {
    pub fn has_full_disk_read_access(&self) -> bool {
        matches!(self, ReadOnlyAccess::FullAccess)
    }

    /// Returns true if platform defaults should be included for restricted read access.
    pub fn include_platform_defaults(&self) -> bool {
        matches!(
            self,
            ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                ..
            }
        )
    }

    /// Returns the readable roots for restricted read access.
    ///
    /// For [`ReadOnlyAccess::FullAccess`], returns an empty list because
    /// callers should grant blanket read access instead.
    pub fn get_readable_roots_with_cwd(&self, cwd: &Path) -> Vec<AbsolutePathBuf> {
        let mut roots: Vec<AbsolutePathBuf> = match self {
            ReadOnlyAccess::FullAccess => return Vec::new(),
            ReadOnlyAccess::Restricted { readable_roots, .. } => {
                let mut roots = readable_roots.clone();
                match AbsolutePathBuf::from_absolute_path(cwd) {
                    Ok(cwd_root) => roots.push(cwd_root),
                    Err(err) => {
                        error!("Ignoring invalid cwd {cwd:?} for sandbox readable root: {err}");
                    }
                }
                roots
            }
        };

        let mut seen = HashSet::new();
        roots.retain(|root| seen.insert(root.to_path_buf()));
        roots
    }
}

/// Determines execution restrictions for model shell commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema, TS)]
#[strum(serialize_all = "kebab-case")]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SandboxPolicy {
    /// No restrictions whatsoever. Use with caution.
    RootAccess,

    /// Read-only access configuration.
    ReadOnly {
        /// Read access granted while running under this policy.
        #[serde(
            default,
            skip_serializing_if = "ReadOnlyAccess::has_full_disk_read_access"
        )]
        access: ReadOnlyAccess,

        /// When set to `true`, outbound network access is allowed. `false` by
        /// default.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        network_access: bool,
    },

    /// Indicates the process is already in an external sandbox. Allows full
    /// disk access while honoring the provided network setting.
    ExternalSandbox {
        /// Whether the external sandbox permits outbound network traffic.
        #[serde(default)]
        network_access: NetworkAccess,
    },

    /// Same as `ReadOnly` but additionally grants write access to the current
    /// working directory ("workspace").
    WorkspaceWrite {
        /// Additional folders (beyond cwd and possibly TMPDIR) that should be
        /// writable from within the sandbox.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        writable_roots: Vec<AbsolutePathBuf>,

        /// Read access granted while running under this policy.
        #[serde(
            default,
            skip_serializing_if = "ReadOnlyAccess::has_full_disk_read_access"
        )]
        read_only_access: ReadOnlyAccess,

        /// When set to `true`, outbound network access is allowed. `false` by
        /// default.
        #[serde(default)]
        network_access: bool,

        /// When set to `true`, will NOT include the per-user `TMPDIR`
        /// environment variable among the default writable roots. Defaults to
        /// `false`.
        #[serde(default)]
        exclude_tmpdir_env_var: bool,

        /// When set to `true`, will NOT include the `/tmp` among the default
        /// writable roots on UNIX. Defaults to `false`.
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

impl std::str::FromStr for SandboxPolicy {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

/// A writable root path accompanied by a list of subpaths that should remain
/// read‑only even when the root is writable. This is primarily used to ensure
/// that folders containing files that could be modified to escalate the
/// privileges of the agent (e.g. `.chaos`, `.git`, notably `.git/hooks`) under
/// a writable root are not modified by the agent.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct WritableRoot {
    pub root: AbsolutePathBuf,

    /// By construction, these subpaths are all under `root`.
    pub read_only_subpaths: Vec<AbsolutePathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false;
        }

        for subpath in &self.read_only_subpaths {
            if path.starts_with(subpath) {
                return false;
            }
        }

        true
    }
}

impl SandboxPolicy {
    /// Returns a policy with read-only disk access and no network.
    pub fn new_read_only_policy() -> Self {
        SandboxPolicy::ReadOnly {
            access: ReadOnlyAccess::FullAccess,
            network_access: false,
        }
    }

    /// Returns a policy that can read the entire disk, but can only write to
    /// the current working directory and the per-user tmp dir on macOS. It does
    /// not allow network access.
    pub fn new_workspace_write_policy() -> Self {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            read_only_access: ReadOnlyAccess::FullAccess,
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        }
    }

    pub fn has_full_disk_read_access(&self) -> bool {
        match self {
            SandboxPolicy::RootAccess => true,
            SandboxPolicy::ExternalSandbox { .. } => true,
            SandboxPolicy::ReadOnly { access, .. } => access.has_full_disk_read_access(),
            SandboxPolicy::WorkspaceWrite {
                read_only_access, ..
            } => read_only_access.has_full_disk_read_access(),
        }
    }

    pub fn has_full_disk_write_access(&self) -> bool {
        match self {
            SandboxPolicy::RootAccess => true,
            SandboxPolicy::ExternalSandbox { .. } => true,
            SandboxPolicy::ReadOnly { .. } => false,
            SandboxPolicy::WorkspaceWrite { .. } => false,
        }
    }

    pub fn has_full_network_access(&self) -> bool {
        match self {
            SandboxPolicy::RootAccess => true,
            SandboxPolicy::ExternalSandbox { network_access } => network_access.is_enabled(),
            SandboxPolicy::ReadOnly { network_access, .. } => *network_access,
            SandboxPolicy::WorkspaceWrite { network_access, .. } => *network_access,
        }
    }

    pub fn include_platform_defaults(&self) -> bool {
        if self.has_full_disk_read_access() {
            return false;
        }
        match self {
            SandboxPolicy::ReadOnly { access, .. } => access.include_platform_defaults(),
            SandboxPolicy::WorkspaceWrite {
                read_only_access, ..
            } => read_only_access.include_platform_defaults(),
            SandboxPolicy::RootAccess | SandboxPolicy::ExternalSandbox { .. } => false,
        }
    }

    pub fn get_readable_roots_with_cwd(&self, cwd: &Path) -> Vec<AbsolutePathBuf> {
        let mut roots = match self {
            SandboxPolicy::RootAccess | SandboxPolicy::ExternalSandbox { .. } => Vec::new(),
            SandboxPolicy::ReadOnly { access, .. } => access.get_readable_roots_with_cwd(cwd),
            SandboxPolicy::WorkspaceWrite {
                read_only_access, ..
            } => {
                let mut roots = read_only_access.get_readable_roots_with_cwd(cwd);
                roots.extend(
                    self.get_writable_roots_with_cwd(cwd)
                        .into_iter()
                        .map(|root| root.root),
                );
                roots
            }
        };
        let mut seen = HashSet::new();
        roots.retain(|root| seen.insert(root.to_path_buf()));
        roots
    }

    pub fn get_writable_roots_with_cwd(&self, cwd: &Path) -> Vec<WritableRoot> {
        match self {
            SandboxPolicy::RootAccess => Vec::new(),
            SandboxPolicy::ExternalSandbox { .. } => Vec::new(),
            SandboxPolicy::ReadOnly { .. } => Vec::new(),
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                read_only_access: _,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                network_access: _,
            } => {
                let mut roots: Vec<AbsolutePathBuf> = writable_roots.clone();

                let cwd_absolute = AbsolutePathBuf::from_absolute_path(cwd);
                match cwd_absolute {
                    Ok(cwd) => roots.push(cwd),
                    Err(e) => {
                        error!(
                            "Ignoring invalid cwd {:?} for sandbox writable root: {}",
                            cwd, e
                        );
                    }
                }

                if cfg!(unix) && !exclude_slash_tmp {
                    #[allow(clippy::expect_used)]
                    let slash_tmp =
                        AbsolutePathBuf::from_absolute_path("/tmp").expect("/tmp is absolute");
                    if slash_tmp.as_path().is_dir() {
                        roots.push(slash_tmp);
                    }
                }

                if !exclude_tmpdir_env_var
                    && let Some(tmpdir) = std::env::var_os("TMPDIR")
                    && !tmpdir.is_empty()
                {
                    match AbsolutePathBuf::from_absolute_path(PathBuf::from(&tmpdir)) {
                        Ok(tmpdir_path) => roots.push(tmpdir_path),
                        Err(e) => {
                            error!(
                                "Ignoring invalid TMPDIR value {tmpdir:?} for sandbox writable root: {e}",
                            );
                        }
                    }
                }

                roots
                    .into_iter()
                    .map(|writable_root| WritableRoot {
                        read_only_subpaths: default_read_only_subpaths_for_writable_root(
                            &writable_root,
                        ),
                        root: writable_root,
                    })
                    .collect()
            }
        }
    }
}

fn default_read_only_subpaths_for_writable_root(
    writable_root: &AbsolutePathBuf,
) -> Vec<AbsolutePathBuf> {
    let mut subpaths: Vec<AbsolutePathBuf> = Vec::new();
    #[allow(clippy::expect_used)]
    let top_level_git = writable_root
        .join(".git")
        .expect(".git is a valid relative path");
    let top_level_git_is_file = top_level_git.as_path().is_file();
    let top_level_git_is_dir = top_level_git.as_path().is_dir();
    if top_level_git_is_dir || top_level_git_is_file {
        if top_level_git_is_file
            && is_git_pointer_file(&top_level_git)
            && let Some(gitdir) = resolve_gitdir_from_file(&top_level_git)
        {
            subpaths.push(gitdir);
        }
        subpaths.push(top_level_git);
    }

    for subdir in &[".agents", ".chaos"] {
        #[allow(clippy::expect_used)]
        let top_level_codex = writable_root.join(subdir).expect("valid relative path");
        if top_level_codex.as_path().is_dir() {
            subpaths.push(top_level_codex);
        }
    }

    let mut deduped = Vec::with_capacity(subpaths.len());
    let mut seen = HashSet::new();
    for path in subpaths {
        if seen.insert(path.to_path_buf()) {
            deduped.push(path);
        }
    }
    deduped
}

fn is_git_pointer_file(path: &AbsolutePathBuf) -> bool {
    path.as_path().is_file() && path.as_path().file_name() == Some(OsStr::new(".git"))
}

fn resolve_gitdir_from_file(dot_git: &AbsolutePathBuf) -> Option<AbsolutePathBuf> {
    let contents = match std::fs::read_to_string(dot_git.as_path()) {
        Ok(contents) => contents,
        Err(err) => {
            error!(
                "Failed to read {path} for gitdir pointer: {err}",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };

    let trimmed = contents.trim();
    let (_, gitdir_raw) = match trimmed.split_once(':') {
        Some(parts) => parts,
        None => {
            error!(
                "Expected {path} to contain a gitdir pointer, but it did not match `gitdir: <path>`.",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };
    let gitdir_raw = gitdir_raw.trim();
    if gitdir_raw.is_empty() {
        error!(
            "Expected {path} to contain a gitdir pointer, but it was empty.",
            path = dot_git.as_path().display()
        );
        return None;
    }
    let base = match dot_git.as_path().parent() {
        Some(base) => base,
        None => {
            error!(
                "Unable to resolve parent directory for {path}.",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };
    let gitdir_path = match AbsolutePathBuf::resolve_path_against_base(gitdir_raw, base) {
        Ok(path) => path,
        Err(err) => {
            error!(
                "Failed to resolve gitdir path {gitdir_raw} from {path}: {err}",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };
    if !gitdir_path.as_path().exists() {
        error!(
            "Resolved gitdir path {path} does not exist.",
            path = gitdir_path.as_path().display()
        );
        return None;
    }
    Some(gitdir_path)
}
