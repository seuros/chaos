//! Warden policy compilation for the hybrid FreeBSD backend.
//!
//! This produces requirements, NOT an executable mount recipe or proof of
//! enforcement. The Warden executor supports only the all-read-only subset.
//! This module does not mount `/` beneath itself, create reserved
//! entries on host writable roots, attach a jail, or execute any command.
//!
//! Sources are resolved for planning only; Warden must pin and
//! validate objects and jail destinations again when applying the plan.

use alcatraz_base::error::{AlcatrazError, Result};
use chaos_ipc::protocol::{SocketPolicy, VfsPolicy};
use chaos_parole::sandbox::{
    has_full_disk_read_access, has_full_disk_write_access, writable_roots,
};
use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NullfsOverlay {
    /// Canonical host source, not an untrusted mount command argument.
    pub source: PathBuf,
    /// Absolute path *within* the future jail, never a host mount destination.
    pub jail_path: PathBuf,
    pub access: OverlayAccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesystemPlan {
    /// No filesystem restriction was requested.
    Unrestricted,
    /// Requires an isolated read-only host view, plus these ordered overlays.
    /// This is not shorthand for a single recursive nullfs mount of `/`.
    ReadOnlyHostView { overlays: Vec<NullfsOverlay> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WardenPlan {
    pub filesystem: FilesystemPlan,
    /// ABI-1 process/network policy. Never represents filesystem enforcement.
    pub credential_label: String,
}

impl WardenPlan {
    pub fn requires_warden(&self) -> bool {
        !matches!(self.filesystem, FilesystemPlan::Unrestricted)
    }
}

fn unsupported(message: impl Into<String>) -> AlcatrazError {
    AlcatrazError::UnsupportedOperation(message.into())
}

/// Compile the initial full-read subset. Unsupported layouts are rejected,
/// rather than dropping a carveout or changing its meaning to make it mountable.
pub fn compile(vfs: &VfsPolicy, network: SocketPolicy, cwd: &Path) -> Result<WardenPlan> {
    if !has_full_disk_read_access(vfs) {
        return Err(unsupported(
            "the initial jail planner supports full-read policies only",
        ));
    }
    let credential_label = crate::gulag::process_label(network);
    if has_full_disk_write_access(vfs) {
        return Ok(WardenPlan {
            filesystem: FilesystemPlan::Unrestricted,
            credential_label,
        });
    }
    validate_path(cwd)?;
    let mut entries = BTreeMap::<PathBuf, OverlayAccess>::new();
    for root in writable_roots(vfs, cwd) {
        let source = existing_source(root.root.as_path())?;
        if source == Path::new("/") {
            return Err(unsupported(
                "a writable alias of / cannot be represented as a restricted overlay",
            ));
        }
        entries.entry(source).or_insert(OverlayAccess::ReadWrite);
        for carveout in root.read_only_subpaths {
            let source = existing_carveout(carveout.as_path()).map_err(|error| {
                unsupported(format!(
                    "cannot plan read-only carveout {}: {error}; \
                     missing mountpoints must not be silently omitted or created on the host",
                    carveout.display()
                ))
            })?;
            entries.insert(source, OverlayAccess::ReadOnly);
        }
    }
    if entries.len() > 256 {
        return Err(unsupported("jail plan exceeds 256 overlays"));
    }
    // A child RW mount would reopen a protected ancestor. Refuse the layout
    // rather than silently granting access or depending on mount order.
    for (path, access) in &entries {
        if *access == OverlayAccess::ReadWrite
            && entries.iter().any(|(other, access)| {
                *access == OverlayAccess::ReadOnly && path.starts_with(other)
            })
        {
            return Err(unsupported(
                "a writable overlay would reopen a read-only ancestor",
            ));
        }
    }
    let mut overlays: Vec<_> = entries
        .into_iter()
        .map(|(source, access)| NullfsOverlay {
            jail_path: source.clone(),
            source,
            access,
        })
        .collect();
    // Parents before children; read-only carveouts cover the writable mount,
    // not the other way round. PathBuf comparisons are component-aware.
    overlays.sort_by(|a, b| {
        a.jail_path
            .components()
            .count()
            .cmp(&b.jail_path.components().count())
            .then_with(|| a.jail_path.cmp(&b.jail_path))
    });
    Ok(WardenPlan {
        filesystem: FilesystemPlan::ReadOnlyHostView { overlays },
        credential_label,
    })
}

fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().contains(&0)
        || path.components().any(|c| c == Component::ParentDir)
    {
        return Err(unsupported(
            "jail policy paths must be absolute and contain no .. or NUL",
        ));
    }
    Ok(())
}

fn existing_source(path: &Path) -> Result<PathBuf> {
    validate_path(path)?;
    let source = std::fs::canonicalize(path)?;
    let metadata = std::fs::metadata(&source)?;
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(unsupported(
            "nullfs policy sources must be existing directories or regular files",
        ));
    }
    Ok(source)
}

fn existing_carveout(path: &Path) -> Result<PathBuf> {
    validate_path(path)?;
    let mut prefix = PathBuf::from("/");
    for component in path.components() {
        if let Component::Normal(name) = component {
            prefix.push(name);
            if std::fs::symlink_metadata(&prefix)?.file_type().is_symlink() {
                return Err(unsupported(
                    "the initial jail planner cannot mask a literal read-only symlink path",
                ));
            }
        }
    }
    existing_source(path)
}

#[cfg(test)]
mod tests;
