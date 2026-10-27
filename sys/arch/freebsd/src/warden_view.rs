//! Warden's initial read-only host view. Does NOT enable writable overlays,
//! execute a child, or replace the privilege/authentication boundary.
//!
//! Host mount boundaries are mirrored explicitly: nullfs is not recursive.
//! Synthetic process/control filesystems are masked; devfs exposes only
//! null/zero/random. No host filesystem or global devfs rule is modified.

use crate::warden_jail::ReadOnlyRootMount;
use crate::warden_mount::{MountRecord, ScopedMounts, snapshot};
use crate::warden_plan::FilesystemPlan;
use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    ReadOnly,
    Devices,
    Mask,
}

/// Provision only an all-read-only plan. The caller must create a jail using
/// the returned pin, apply process/network policy, and retain ownership until
/// all child processes have been removed before cleaning up these mounts.
pub fn provision(
    plan: &FilesystemPlan,
    parent: &Path,
) -> io::Result<(ScopedMounts, ReadOnlyRootMount)> {
    match plan {
        FilesystemPlan::ReadOnlyHostView { overlays } if overlays.is_empty() => {}
        _ => {
            return Err(unsupported(
                "this view builder supports read-only host views without overlays only",
            ));
        }
    }
    let before = snapshot()?;
    let recipe = classify(&before)?;
    let mut mounts = ScopedMounts::create(parent)?;
    for (mount, action) in recipe {
        let relative = mount
            .target
            .strip_prefix("/")
            .map_err(|_| unsupported("invalid host mount path"))?;
        match action {
            Action::ReadOnly => {
                mounts.nullfs_checked(&mount.target, relative, true, Some(mount.id))?
            }
            Action::Devices => mounts.minimal_devices(relative)?,
            Action::Mask => mounts.empty_mask(relative)?,
        }
    }
    // Detect mount-table changes during setup rather than silently presenting
    // a stale/partial host view. Scoped additions are the only expected change.
    let mut after: Vec<_> = snapshot()?
        .into_iter()
        .filter(|mount| !mount.target.starts_with(mounts.state_path()))
        .collect();
    let mut before = before;
    before.sort_by_key(|mount| mount.id);
    after.sort_by_key(|mount| mount.id);
    if before != after {
        return Err(unsupported(
            "host mount table changed while constructing jail view",
        ));
    }
    let root = ReadOnlyRootMount::open(&mounts.view_path())?;
    Ok((mounts, root))
}

fn classify(table: &[MountRecord]) -> io::Result<Vec<(&MountRecord, Action)>> {
    if table.len() > 256 {
        return Err(unsupported("host mount table exceeds initial view limit"));
    }
    let mut sorted: Vec<_> = table.iter().collect();
    sorted.sort_by(|left, right| {
        left.target
            .components()
            .count()
            .cmp(&right.target.components().count())
            .then(left.target.cmp(&right.target))
    });
    let mut seen = BTreeSet::new();
    let mut masked = Vec::new();
    let mut result = Vec::new();
    for mount in sorted {
        if !mount.target.is_absolute()
            || mount
                .target
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
            || !seen.insert(&mount.target)
        {
            return Err(unsupported("ambiguous or invalid host mount path"));
        }
        if masked
            .iter()
            .any(|parent: &&Path| mount.target.starts_with(parent))
        {
            continue;
        }
        let action = match mount.filesystem_type.as_str() {
            "zfs" | "ufs" | "tmpfs" | "nfs" | "msdosfs" | "cd9660" | "ext2fs" => Action::ReadOnly,
            "devfs" if mount.target != Path::new("/") => Action::Devices,
            "procfs" | "linprocfs" | "linsysfs" | "fdescfs" if mount.target != Path::new("/") => {
                Action::Mask
            }
            _ => return Err(unsupported("unsupported host filesystem in read-only view")),
        };
        if action != Action::ReadOnly {
            masked.push(mount.target.as_path());
        }
        result.push((mount, action));
    }
    if result
        .first()
        .map(|(mount, action)| (mount.target.as_path(), *action))
        != Some((Path::new("/"), Action::ReadOnly))
    {
        return Err(unsupported("no supported host root mount"));
    }
    Ok(result)
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::warden_mount::MountId;
    use std::path::PathBuf;

    fn mount(id: i32, path: &str, kind: &str) -> MountRecord {
        MountRecord {
            id: MountId([id, 0]),
            target: PathBuf::from(path),
            filesystem_type: kind.into(),
            mounted_from: PathBuf::from("fixture"),
            flags: 0,
        }
    }

    #[test]
    fn boundaries_are_parent_first_and_synthetic_descendants_never_reopen_masks() {
        let table = [
            mount(4, "/dev/fd", "fdescfs"),
            mount(3, "/home", "zfs"),
            mount(2, "/dev", "devfs"),
            mount(1, "/", "zfs"),
            mount(5, "/dev/shm", "tmpfs"),
            mount(6, "/proc", "procfs"),
        ];
        let recipe = classify(&table).unwrap();
        assert_eq!(
            recipe
                .iter()
                .map(|(mount, action)| (mount.target.as_path(), *action))
                .collect::<Vec<_>>(),
            vec![
                (Path::new("/"), Action::ReadOnly),
                (Path::new("/dev"), Action::Devices),
                (Path::new("/home"), Action::ReadOnly),
                (Path::new("/proc"), Action::Mask)
            ]
        );
    }

    #[test]
    fn ambiguous_and_unsupported_host_views_are_refused() {
        for table in [
            vec![mount(1, "/", "zfs"), mount(2, "/", "zfs")],
            vec![mount(1, "/", "devfs")],
            vec![mount(1, "/", "zfs"), mount(2, "/strange", "unknown")],
            vec![mount(1, "/", "zfs"), mount(2, "/bad/../path", "zfs")],
            vec![mount(1, "relative", "zfs")],
        ] {
            assert!(classify(&table).is_err());
        }
    }
}
