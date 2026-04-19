use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use chaos_realpath::AbsolutePathBuf;
use tracing::error;

use super::SocketPolicy;
use super::VfsAccessMode;
use super::VfsEntry;
use super::VfsPath;
use super::VfsPolicy;
use super::VfsPolicyKind;
use super::VfsSpecialPath;
use crate::protocol::NetworkAccess;
use crate::protocol::ReadOnlyAccess;
use crate::protocol::SandboxPolicy;
use crate::protocol::WritableRoot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedVfsEntry {
    pub(super) path: AbsolutePathBuf,
    pub(super) access: VfsAccessMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VfsSemanticSignature {
    pub(super) has_full_disk_read_access: bool,
    pub(super) has_full_disk_write_access: bool,
    pub(super) include_platform_defaults: bool,
    pub(super) readable_roots: Vec<AbsolutePathBuf>,
    pub(super) writable_roots: Vec<WritableRoot>,
    pub(super) unreadable_roots: Vec<AbsolutePathBuf>,
}

impl VfsPolicy {
    pub(super) fn has_root_access(&self, predicate: impl Fn(VfsAccessMode) -> bool) -> bool {
        matches!(self.kind, VfsPolicyKind::Restricted)
            && self.entries.iter().any(|entry| {
                matches!(
                    &entry.path,
                    VfsPath::Special { value }
                        if matches!(value, VfsSpecialPath::Root) && predicate(entry.access)
                )
            })
    }

    pub(super) fn has_explicit_deny_entries(&self) -> bool {
        matches!(self.kind, VfsPolicyKind::Restricted)
            && self
                .entries
                .iter()
                .any(|entry| entry.access == VfsAccessMode::None)
    }

    /// Returns true when a restricted policy contains any entry that really
    /// reduces a broader `:root = write` grant.
    ///
    /// Raw entry presence is not enough here: an equally specific `write`
    /// entry for the same target wins under the normal precedence rules, so a
    /// shadowed `read` entry must not downgrade the policy out of full-disk
    /// write mode.
    pub(super) fn has_write_narrowing_entries(&self) -> bool {
        matches!(self.kind, VfsPolicyKind::Restricted)
            && self.entries.iter().any(|entry| {
                if entry.access.can_write() {
                    return false;
                }

                match &entry.path {
                    VfsPath::Path { .. } => !self.has_same_target_write_override(entry),
                    VfsPath::Special { value } => match value {
                        VfsSpecialPath::Root => entry.access == VfsAccessMode::None,
                        VfsSpecialPath::Minimal | VfsSpecialPath::Unknown { .. } => false,
                        _ => !self.has_same_target_write_override(entry),
                    },
                }
            })
    }

    /// Returns true when a higher-priority `write` entry targets the same
    /// location as `entry`, so `entry` cannot narrow effective write access.
    pub(super) fn has_same_target_write_override(&self, entry: &VfsEntry) -> bool {
        self.entries.iter().any(|candidate| {
            candidate.access.can_write()
                && candidate.access > entry.access
                && vfs_paths_share_target(&candidate.path, &entry.path)
        })
    }

    /// Converts a sandbox policy into an equivalent filesystem policy for the
    /// provided cwd.
    ///
    /// `WorkspaceWrite` policies may list readable roots that live under an
    /// already-writable root. Those paths are redundant in the high-level
    /// policy and must not become read-only carveouts when projected into the
    /// filesystem policy.
    pub fn from_sandbox_policy(sandbox_policy: &SandboxPolicy, cwd: &Path) -> Self {
        let mut file_system_policy = Self::from(sandbox_policy);
        if matches!(sandbox_policy, SandboxPolicy::WorkspaceWrite { .. }) {
            let writable_roots = sandbox_policy.get_writable_roots_with_cwd(cwd);
            file_system_policy.entries.retain(|entry| {
                if entry.access != VfsAccessMode::Read {
                    return true;
                }

                match &entry.path {
                    VfsPath::Path { path } => !writable_roots
                        .iter()
                        .any(|root| root.is_path_writable(path.as_path())),
                    VfsPath::Special { .. } => true,
                }
            });
        }

        file_system_policy
    }

    /// Returns true when filesystem reads are unrestricted.
    pub fn has_full_disk_read_access(&self) -> bool {
        match self.kind {
            VfsPolicyKind::Unrestricted | VfsPolicyKind::ExternalSandbox => true,
            VfsPolicyKind::Restricted => {
                self.has_root_access(VfsAccessMode::can_read) && !self.has_explicit_deny_entries()
            }
        }
    }

    /// Returns true when filesystem writes are unrestricted.
    pub fn has_full_disk_write_access(&self) -> bool {
        match self.kind {
            VfsPolicyKind::Unrestricted | VfsPolicyKind::ExternalSandbox => true,
            VfsPolicyKind::Restricted => {
                self.has_root_access(VfsAccessMode::can_write)
                    && !self.has_write_narrowing_entries()
            }
        }
    }

    /// Returns true when platform-default readable roots should be included.
    pub fn include_platform_defaults(&self) -> bool {
        !self.has_full_disk_read_access()
            && matches!(self.kind, VfsPolicyKind::Restricted)
            && self.entries.iter().any(|entry| {
                matches!(
                    &entry.path,
                    VfsPath::Special { value }
                        if matches!(value, VfsSpecialPath::Minimal)
                            && entry.access.can_read()
                )
            })
    }

    pub fn resolve_access_with_cwd(&self, path: &Path, cwd: &Path) -> VfsAccessMode {
        match self.kind {
            VfsPolicyKind::Unrestricted | VfsPolicyKind::ExternalSandbox => {
                return VfsAccessMode::Write;
            }
            VfsPolicyKind::Restricted => {}
        }

        let Some(path) = resolve_candidate_path(path, cwd) else {
            return VfsAccessMode::None;
        };

        self.resolved_entries_with_cwd(cwd)
            .into_iter()
            .filter(|entry| path.as_path().starts_with(entry.path.as_path()))
            .max_by_key(resolved_entry_precedence)
            .map(|entry| entry.access)
            .unwrap_or(VfsAccessMode::None)
    }

    pub fn can_read_path_with_cwd(&self, path: &Path, cwd: &Path) -> bool {
        self.resolve_access_with_cwd(path, cwd).can_read()
    }

    pub fn can_write_path_with_cwd(&self, path: &Path, cwd: &Path) -> bool {
        self.resolve_access_with_cwd(path, cwd).can_write()
    }

    pub fn needs_direct_runtime_enforcement(
        &self,
        network_policy: SocketPolicy,
        cwd: &Path,
    ) -> bool {
        if !matches!(self.kind, VfsPolicyKind::Restricted) {
            return false;
        }

        let Ok(sandbox_policy) = self.to_sandbox_policy(network_policy, cwd) else {
            return true;
        };

        self.semantic_signature(cwd)
            != VfsPolicy::from_sandbox_policy(&sandbox_policy, cwd).semantic_signature(cwd)
    }

    /// Returns the explicit readable roots resolved against the provided cwd.
    pub fn get_readable_roots_with_cwd(&self, cwd: &Path) -> Vec<AbsolutePathBuf> {
        if self.has_full_disk_read_access() {
            return Vec::new();
        }

        dedup_absolute_paths(
            self.resolved_entries_with_cwd(cwd)
                .into_iter()
                .filter(|entry| entry.access.can_read())
                .filter(|entry| self.can_read_path_with_cwd(entry.path.as_path(), cwd))
                .map(|entry| entry.path)
                .collect(),
            /*normalize_effective_paths*/ true,
        )
    }

    /// Returns the writable roots together with read-only carveouts resolved
    /// against the provided cwd.
    pub fn get_writable_roots_with_cwd(&self, cwd: &Path) -> Vec<WritableRoot> {
        if self.has_full_disk_write_access() {
            return Vec::new();
        }

        let resolved_entries = self.resolved_entries_with_cwd(cwd);
        let writable_entries: Vec<AbsolutePathBuf> = resolved_entries
            .iter()
            .filter(|entry| entry.access.can_write())
            .filter(|entry| self.can_write_path_with_cwd(entry.path.as_path(), cwd))
            .map(|entry| entry.path.clone())
            .collect();

        dedup_absolute_paths(
            writable_entries.clone(),
            /*normalize_effective_paths*/ true,
        )
        .into_iter()
        .map(|root| {
            // Filesystem-root policies stay in their effective canonical form
            // so root-wide aliases do not create duplicate top-level masks.
            // Example: keep `/var/...` normalized under `/` instead of
            // materializing both `/var/...` and `/private/var/...`.
            let preserve_raw_carveout_paths = root.as_path().parent().is_some();
            let raw_writable_roots: Vec<&AbsolutePathBuf> = writable_entries
                .iter()
                .filter(|path| normalize_effective_absolute_path((*path).clone()) == root)
                .collect();
            let mut read_only_subpaths = default_read_only_subpaths_for_writable_root(&root);
            // Narrower explicit non-write entries carve out broader writable roots.
            // More specific write entries still remain writable because they appear
            // as separate WritableRoot values and are checked independently.
            // Preserve symlink path components that live under the writable root
            // so downstream sandboxes can still mask the symlink inode itself.
            // Example: if `<root>/.chaos -> <root>/decoy`, bwrap must still see
            // `<root>/.chaos`, not only the resolved `<root>/decoy`.
            read_only_subpaths.extend(
                resolved_entries
                    .iter()
                    .filter(|entry| !entry.access.can_write())
                    .filter(|entry| !self.can_write_path_with_cwd(entry.path.as_path(), cwd))
                    .filter_map(|entry| {
                        let effective_path = normalize_effective_absolute_path(entry.path.clone());
                        // Preserve the literal in-root path whenever the
                        // carveout itself lives under this writable root, even
                        // if following symlinks would resolve back to the root
                        // or escape outside it. Downstream sandboxes need that
                        // raw path so they can mask the symlink inode itself.
                        // Examples:
                        // - `<root>/linked-private -> <root>/decoy-private`
                        // - `<root>/linked-private -> /tmp/outside-private`
                        // - `<root>/alias-root -> <root>`
                        let raw_carveout_path = if preserve_raw_carveout_paths {
                            if entry.path == root {
                                None
                            } else if entry.path.as_path().starts_with(root.as_path()) {
                                Some(entry.path.clone())
                            } else {
                                raw_writable_roots.iter().find_map(|raw_root| {
                                    let suffix = entry
                                        .path
                                        .as_path()
                                        .strip_prefix(raw_root.as_path())
                                        .ok()?;
                                    if suffix.as_os_str().is_empty() {
                                        return None;
                                    }
                                    root.join(suffix).ok()
                                })
                            }
                        } else {
                            None
                        };

                        if let Some(raw_carveout_path) = raw_carveout_path {
                            return Some(raw_carveout_path);
                        }

                        if effective_path == root
                            || !effective_path.as_path().starts_with(root.as_path())
                        {
                            return None;
                        }

                        Some(effective_path)
                    }),
            );
            WritableRoot {
                root,
                // Preserve literal in-root protected paths like `.git` and
                // `.chaos` so downstream sandboxes can still detect and mask
                // the symlink itself instead of only its resolved target.
                read_only_subpaths: dedup_absolute_paths(
                    read_only_subpaths,
                    /*normalize_effective_paths*/ false,
                ),
            }
        })
        .collect()
    }

    /// Returns explicit unreadable roots resolved against the provided cwd.
    pub fn get_unreadable_roots_with_cwd(&self, cwd: &Path) -> Vec<AbsolutePathBuf> {
        if !matches!(self.kind, VfsPolicyKind::Restricted) {
            return Vec::new();
        }

        let root = AbsolutePathBuf::from_absolute_path(cwd)
            .ok()
            .map(|cwd| absolute_vfs_root_path_for_cwd(&cwd));

        dedup_absolute_paths(
            self.resolved_entries_with_cwd(cwd)
                .iter()
                .filter(|entry| entry.access == VfsAccessMode::None)
                .filter(|entry| !self.can_read_path_with_cwd(entry.path.as_path(), cwd))
                // Restricted policies already deny reads outside explicit allow roots,
                // so materializing the filesystem root here would erase narrower
                // readable carveouts when downstream sandboxes apply deny masks last.
                .filter(|entry| root.as_ref() != Some(&entry.path))
                .map(|entry| entry.path.clone())
                .collect(),
            /*normalize_effective_paths*/ true,
        )
    }

    pub fn to_sandbox_policy(
        &self,
        network_policy: SocketPolicy,
        cwd: &Path,
    ) -> io::Result<SandboxPolicy> {
        Ok(match self.kind {
            VfsPolicyKind::ExternalSandbox => SandboxPolicy::ExternalSandbox {
                network_access: if network_policy.is_enabled() {
                    NetworkAccess::Enabled
                } else {
                    NetworkAccess::Restricted
                },
            },
            VfsPolicyKind::Unrestricted => {
                if network_policy.is_enabled() {
                    SandboxPolicy::RootAccess
                } else {
                    SandboxPolicy::ExternalSandbox {
                        network_access: NetworkAccess::Restricted,
                    }
                }
            }
            VfsPolicyKind::Restricted => {
                let cwd_absolute = AbsolutePathBuf::from_absolute_path(cwd).ok();
                let mut include_platform_defaults = false;
                let mut has_full_disk_read_access = false;
                let mut has_full_disk_write_access = false;
                let mut workspace_root_writable = false;
                let mut writable_roots = Vec::new();
                let mut readable_roots = Vec::new();
                let mut tmpdir_writable = false;
                let mut slash_tmp_writable = false;

                for entry in &self.entries {
                    match &entry.path {
                        VfsPath::Path { path } => {
                            if entry.access.can_write() {
                                if cwd_absolute.as_ref().is_some_and(|cwd| cwd == path) {
                                    workspace_root_writable = true;
                                } else {
                                    writable_roots.push(path.clone());
                                }
                            } else if entry.access.can_read() {
                                readable_roots.push(path.clone());
                            }
                        }
                        VfsPath::Special { value } => match value {
                            VfsSpecialPath::Root => match entry.access {
                                VfsAccessMode::None => {}
                                VfsAccessMode::Read => has_full_disk_read_access = true,
                                VfsAccessMode::Write => {
                                    has_full_disk_read_access = true;
                                    has_full_disk_write_access = true;
                                }
                            },
                            VfsSpecialPath::Minimal => {
                                if entry.access.can_read() {
                                    include_platform_defaults = true;
                                }
                            }
                            VfsSpecialPath::CurrentWorkingDirectory => {
                                if entry.access.can_write() {
                                    workspace_root_writable = true;
                                } else if entry.access.can_read()
                                    && let Some(path) =
                                        resolve_vfs_special_path(value, cwd_absolute.as_ref())
                                {
                                    readable_roots.push(path);
                                }
                            }
                            VfsSpecialPath::ProjectRoots { subpath } => {
                                if subpath.is_none() && entry.access.can_write() {
                                    workspace_root_writable = true;
                                } else if let Some(path) =
                                    resolve_vfs_special_path(value, cwd_absolute.as_ref())
                                {
                                    if entry.access.can_write() {
                                        writable_roots.push(path);
                                    } else if entry.access.can_read() {
                                        readable_roots.push(path);
                                    }
                                }
                            }
                            VfsSpecialPath::Tmpdir => {
                                if entry.access.can_write() {
                                    tmpdir_writable = true;
                                } else if entry.access.can_read()
                                    && let Some(path) =
                                        resolve_vfs_special_path(value, cwd_absolute.as_ref())
                                {
                                    readable_roots.push(path);
                                }
                            }
                            VfsSpecialPath::SlashTmp => {
                                if entry.access.can_write() {
                                    slash_tmp_writable = true;
                                } else if entry.access.can_read()
                                    && let Some(path) =
                                        resolve_vfs_special_path(value, cwd_absolute.as_ref())
                                {
                                    readable_roots.push(path);
                                }
                            }
                            VfsSpecialPath::Unknown { .. } => {}
                        },
                    }
                }

                if has_full_disk_write_access {
                    return Ok(if network_policy.is_enabled() {
                        SandboxPolicy::RootAccess
                    } else {
                        SandboxPolicy::ExternalSandbox {
                            network_access: NetworkAccess::Restricted,
                        }
                    });
                }

                let read_only_access = if has_full_disk_read_access {
                    ReadOnlyAccess::FullAccess
                } else {
                    ReadOnlyAccess::Restricted {
                        include_platform_defaults,
                        readable_roots: dedup_absolute_paths(
                            readable_roots,
                            /*normalize_effective_paths*/ false,
                        ),
                    }
                };

                if workspace_root_writable {
                    SandboxPolicy::WorkspaceWrite {
                        writable_roots: dedup_absolute_paths(
                            writable_roots,
                            /*normalize_effective_paths*/ false,
                        ),
                        read_only_access,
                        network_access: network_policy.is_enabled(),
                        exclude_tmpdir_env_var: !tmpdir_writable,
                        exclude_slash_tmp: !slash_tmp_writable,
                    }
                } else if !writable_roots.is_empty() || tmpdir_writable || slash_tmp_writable {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "permissions profile requests filesystem writes outside the workspace root, which is not supported until the runtime enforces VfsPolicy directly",
                    ));
                } else {
                    SandboxPolicy::ReadOnly {
                        access: read_only_access,
                        network_access: network_policy.is_enabled(),
                    }
                }
            }
        })
    }

    pub(super) fn resolved_entries_with_cwd(&self, cwd: &Path) -> Vec<ResolvedVfsEntry> {
        let cwd_absolute = AbsolutePathBuf::from_absolute_path(cwd).ok();
        self.entries
            .iter()
            .filter_map(|entry| {
                resolve_entry_path(&entry.path, cwd_absolute.as_ref()).map(|path| {
                    ResolvedVfsEntry {
                        path,
                        access: entry.access,
                    }
                })
            })
            .collect()
    }

    pub(super) fn semantic_signature(&self, cwd: &Path) -> VfsSemanticSignature {
        VfsSemanticSignature {
            has_full_disk_read_access: self.has_full_disk_read_access(),
            has_full_disk_write_access: self.has_full_disk_write_access(),
            include_platform_defaults: self.include_platform_defaults(),
            readable_roots: self.get_readable_roots_with_cwd(cwd),
            writable_roots: self.get_writable_roots_with_cwd(cwd),
            unreadable_roots: self.get_unreadable_roots_with_cwd(cwd),
        }
    }
}

fn resolve_vfs_path(path: &VfsPath, cwd: Option<&AbsolutePathBuf>) -> Option<AbsolutePathBuf> {
    match path {
        VfsPath::Path { path } => Some(path.clone()),
        VfsPath::Special { value } => resolve_vfs_special_path(value, cwd),
    }
}

fn resolve_entry_path(path: &VfsPath, cwd: Option<&AbsolutePathBuf>) -> Option<AbsolutePathBuf> {
    match path {
        VfsPath::Special {
            value: VfsSpecialPath::Root,
        } => cwd.map(absolute_vfs_root_path_for_cwd),
        _ => resolve_vfs_path(path, cwd),
    }
}

fn resolve_candidate_path(path: &Path, cwd: &Path) -> Option<AbsolutePathBuf> {
    if path.is_absolute() {
        AbsolutePathBuf::from_absolute_path(path).ok()
    } else {
        AbsolutePathBuf::resolve_path_against_base(path, cwd).ok()
    }
}

/// Returns true when two config paths refer to the same exact target before
/// any prefix matching is applied.
///
/// This is intentionally narrower than full path resolution: it only answers
/// the "can one entry shadow another at the same specificity?" question used
/// by `has_write_narrowing_entries`.
fn vfs_paths_share_target(left: &VfsPath, right: &VfsPath) -> bool {
    match (left, right) {
        (VfsPath::Path { path: left }, VfsPath::Path { path: right }) => left == right,
        (VfsPath::Special { value: left }, VfsPath::Special { value: right }) => {
            special_paths_share_target(left, right)
        }
        (VfsPath::Path { path }, VfsPath::Special { value })
        | (VfsPath::Special { value }, VfsPath::Path { path }) => {
            special_path_matches_absolute_path(value, path)
        }
    }
}

/// Compares special-path tokens that resolve to the same concrete target
/// without needing a cwd.
fn special_paths_share_target(left: &VfsSpecialPath, right: &VfsSpecialPath) -> bool {
    match (left, right) {
        (VfsSpecialPath::Root, VfsSpecialPath::Root)
        | (VfsSpecialPath::Minimal, VfsSpecialPath::Minimal)
        | (VfsSpecialPath::CurrentWorkingDirectory, VfsSpecialPath::CurrentWorkingDirectory)
        | (VfsSpecialPath::Tmpdir, VfsSpecialPath::Tmpdir)
        | (VfsSpecialPath::SlashTmp, VfsSpecialPath::SlashTmp) => true,
        (
            VfsSpecialPath::CurrentWorkingDirectory,
            VfsSpecialPath::ProjectRoots { subpath: None },
        )
        | (
            VfsSpecialPath::ProjectRoots { subpath: None },
            VfsSpecialPath::CurrentWorkingDirectory,
        ) => true,
        (
            VfsSpecialPath::ProjectRoots { subpath: left },
            VfsSpecialPath::ProjectRoots { subpath: right },
        ) => left == right,
        (
            VfsSpecialPath::Unknown {
                path: left,
                subpath: left_subpath,
            },
            VfsSpecialPath::Unknown {
                path: right,
                subpath: right_subpath,
            },
        ) => left == right && left_subpath == right_subpath,
        _ => false,
    }
}

/// Matches cwd-independent special paths against absolute `Path` entries when
/// they name the same location.
///
/// We intentionally only fold the special paths whose concrete meaning is
/// stable without a cwd, such as `/` and `/tmp`.
fn special_path_matches_absolute_path(value: &VfsSpecialPath, path: &AbsolutePathBuf) -> bool {
    match value {
        VfsSpecialPath::Root => path.as_path().parent().is_none(),
        VfsSpecialPath::SlashTmp => path.as_path() == Path::new("/tmp"),
        _ => false,
    }
}

/// Orders resolved entries so the most specific path wins first, then applies
/// the access tie-breaker from [`VfsAccessMode`].
fn resolved_entry_precedence(entry: &ResolvedVfsEntry) -> (usize, VfsAccessMode) {
    let specificity = entry.path.as_path().components().count();
    (specificity, entry.access)
}

pub fn absolute_vfs_root_path_for_cwd(cwd: &AbsolutePathBuf) -> AbsolutePathBuf {
    let root = cwd
        .as_path()
        .ancestors()
        .last()
        .unwrap_or_else(|| panic!("cwd must have a filesystem root"));
    AbsolutePathBuf::from_absolute_path(root)
        .unwrap_or_else(|err| panic!("cwd root must be an absolute path: {err}"))
}

fn resolve_vfs_special_path(
    value: &VfsSpecialPath,
    cwd: Option<&AbsolutePathBuf>,
) -> Option<AbsolutePathBuf> {
    match value {
        VfsSpecialPath::Root | VfsSpecialPath::Minimal | VfsSpecialPath::Unknown { .. } => None,
        VfsSpecialPath::CurrentWorkingDirectory => {
            let cwd = cwd?;
            Some(cwd.clone())
        }
        VfsSpecialPath::ProjectRoots { subpath } => {
            let cwd = cwd?;
            match subpath.as_ref() {
                Some(subpath) => {
                    AbsolutePathBuf::resolve_path_against_base(subpath, cwd.as_path()).ok()
                }
                None => Some(cwd.clone()),
            }
        }
        VfsSpecialPath::Tmpdir => {
            let tmpdir = std::env::var_os("TMPDIR")?;
            if tmpdir.is_empty() {
                None
            } else {
                let tmpdir = AbsolutePathBuf::from_absolute_path(PathBuf::from(tmpdir)).ok()?;
                Some(tmpdir)
            }
        }
        VfsSpecialPath::SlashTmp => {
            #[allow(clippy::expect_used)]
            let slash_tmp = AbsolutePathBuf::from_absolute_path("/tmp").expect("/tmp is absolute");
            if !slash_tmp.as_path().is_dir() {
                return None;
            }
            Some(slash_tmp)
        }
    }
}

pub(super) fn dedup_absolute_paths(
    paths: Vec<AbsolutePathBuf>,
    normalize_effective_paths: bool,
) -> Vec<AbsolutePathBuf> {
    let mut deduped = Vec::with_capacity(paths.len());
    let mut seen = HashSet::new();
    for path in paths {
        let dedup_path = if normalize_effective_paths {
            normalize_effective_absolute_path(path)
        } else {
            path
        };
        if seen.insert(dedup_path.to_path_buf()) {
            deduped.push(dedup_path);
        }
    }
    deduped
}

fn normalize_effective_absolute_path(path: AbsolutePathBuf) -> AbsolutePathBuf {
    let raw_path = path.to_path_buf();
    for ancestor in raw_path.ancestors() {
        let Ok(canonical_ancestor) = ancestor.canonicalize() else {
            continue;
        };
        let Ok(suffix) = raw_path.strip_prefix(ancestor) else {
            continue;
        };
        if let Ok(normalized_path) =
            AbsolutePathBuf::from_absolute_path(canonical_ancestor.join(suffix))
        {
            return normalized_path;
        }
    }
    path
}

fn default_read_only_subpaths_for_writable_root(
    writable_root: &AbsolutePathBuf,
) -> Vec<AbsolutePathBuf> {
    let mut subpaths: Vec<AbsolutePathBuf> = Vec::new();
    #[allow(clippy::expect_used)]
    let top_level_git = writable_root
        .join(".git")
        .expect(".git is a valid relative path");
    // This applies to typical repos (directory .git), worktrees/submodules
    // (file .git with gitdir pointer), and bare repos when the gitdir is the
    // writable root itself.
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

    // Make .agents/skills and the project config folder (.chaos) read-only to
    // the agent, by default.
    for subdir in &[".agents", ".chaos"] {
        #[allow(clippy::expect_used)]
        let top_level_codex = writable_root.join(subdir).expect("valid relative path");
        if top_level_codex.as_path().is_dir() {
            subpaths.push(top_level_codex);
        }
    }

    dedup_absolute_paths(subpaths, /*normalize_effective_paths*/ false)
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
