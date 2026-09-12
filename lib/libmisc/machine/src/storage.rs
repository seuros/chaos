use serde::Serialize;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageRole {
    Workspace,
    State,
    Temporary,
    Output,
    Checkpoint,
}

/// A path the task actually uses. There are no implicit paths or drive scans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageTarget {
    pub role: StorageRole,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedStorageTarget {
    pub target: StorageTarget,
    /// Canonical existing path used for the probe. For a not-yet-created output,
    /// this is its closest existing ancestor, not necessarily the output itself.
    pub probed_path: PathBuf,
}

/// Namespace-local filesystem identity, not a physical SSD or global stable ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FilesystemId {
    pub device: u64,
    pub filesystem: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemBacking {
    Memory,
    /// Not a known memory filesystem. Does NOT guarantee persistence: a
    /// temporary directory, container layer, or RAM-backed block device can
    /// still disappear.
    Other,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Filesystem {
    pub id: FilesystemId,
    pub filesystem_type: Option<String>,
    pub backing: FilesystemBacking,
    pub total_bytes: u64,
    /// statvfs's bavail: space available to an unprivileged caller, excluding
    /// reserved blocks. Not a reservation or a per-user quota guarantee.
    pub available_bytes: u64,
    /// None when the filesystem does not expose inode accounting.
    pub available_inodes: Option<u64>,
    /// Mount flag, not proof that a particular path is writable.
    pub read_only: bool,
    /// Multiple task paths on the same filesystem share one observation.
    pub targets: Vec<ResolvedStorageTarget>,
}

impl Filesystem {
    /// None for filesystems without a meaningful total (e.g. some pseudo-fs).
    pub fn available_percent(&self) -> Option<f64> {
        (self.total_bytes > 0 && self.available_bytes <= self.total_bytes)
            .then(|| self.available_bytes as f64 * 100.0 / self.total_bytes as f64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageFailure {
    pub target: StorageTarget,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageSnapshot {
    pub observed_at: SystemTime,
    pub filesystems: Vec<Filesystem>,
    /// Failure to probe is not zero free space and is not a healthy disk.
    pub unavailable: Vec<StorageFailure>,
}

/// Probe only explicitly relevant paths, resolving symlinks and nonexistent
/// output paths to their actual filesystem. Deduplicate by filesystem identity
/// and read-only view (read-only bind mounts must not inherit a writable view).
///
/// Absolute paths are required. Permission errors, dangling symlinks, and
/// ambiguous missing paths containing `..` are reported, not guessed around.
pub fn inspect_storage(targets: &[StorageTarget]) -> StorageSnapshot {
    let mut snapshot = StorageSnapshot {
        observed_at: SystemTime::now(),
        filesystems: Vec::new(),
        unavailable: Vec::new(),
    };
    for target in targets {
        let result = existing_path(&target.path)
            .and_then(|path| probe(&path).map(|filesystem| (path, filesystem)));
        match result {
            Ok((probed_path, filesystem)) => snapshot.insert(
                ResolvedStorageTarget {
                    target: target.clone(),
                    probed_path,
                },
                filesystem,
            ),
            Err(error) => snapshot.unavailable.push(StorageFailure {
                target: target.clone(),
                reason: error.to_string(),
            }),
        }
    }
    snapshot
}

impl StorageSnapshot {
    fn insert(&mut self, target: ResolvedStorageTarget, mut filesystem: Filesystem) {
        if let Some(existing) = self.filesystems.iter_mut().find(|existing| {
            existing.id == filesystem.id && existing.read_only == filesystem.read_only
        }) {
            if !existing.targets.contains(&target) {
                existing.targets.push(target);
            }
        } else {
            filesystem.targets.push(target);
            self.filesystems.push(filesystem);
        }
    }
}

fn existing_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "storage paths must be absolute",
        ));
    }
    let mut candidate = path.to_path_buf();
    loop {
        match fs::canonicalize(&candidate) {
            Ok(resolved) => return Ok(resolved),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A dangling symlink is an existing object, not a missing child
                // on its parent's filesystem. Never substitute that parent.
                match fs::symlink_metadata(&candidate) {
                    Ok(_) => return Err(error),
                    Err(metadata_error) if metadata_error.kind() == io::ErrorKind::NotFound => {}
                    Err(metadata_error) => return Err(metadata_error),
                }
                if path.components().any(|part| part == Component::ParentDir) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cannot resolve a missing storage path containing '..'",
                    ));
                }
                if !candidate.pop() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[allow(
    clippy::unnecessary_cast,
    reason = "statvfs field widths differ across Unix targets"
)]
fn probe(path: &Path) -> io::Result<Filesystem> {
    let metadata = fs::metadata(path)?;
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: statvfs is a C output struct with integer fields.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: path is NUL-terminated and stat is a valid output pointer.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let block_size = stat.f_frsize as u64;
    if block_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero filesystem block size",
        ));
    }
    let bytes = |blocks: u64| {
        blocks.checked_mul(block_size).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "filesystem byte count overflow")
        })
    };
    let (filesystem_type, backing) = filesystem_kind(&c_path);
    Ok(Filesystem {
        id: FilesystemId {
            device: metadata.dev(),
            filesystem: stat.f_fsid as u64,
        },
        filesystem_type,
        backing,
        total_bytes: bytes(stat.f_blocks as u64)?,
        available_bytes: bytes(stat.f_bavail as u64)?,
        available_inodes: (stat.f_files > 0).then_some(stat.f_favail as u64),
        read_only: stat.f_flag & libc::ST_RDONLY != 0,
        targets: Vec::new(),
    })
}

fn filesystem_kind(path: &std::ffi::CStr) -> (Option<String>, FilesystemBacking) {
    // SAFETY: statfs is a C output struct; path and output pointers are valid.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut stat) } != 0 {
        return (None, FilesystemBacking::Unknown);
    }
    #[cfg(target_os = "linux")]
    let name = match stat.f_type as u64 {
        0x0102_1994 => "tmpfs",
        0x8584_58f6 => "ramfs",
        0xef53 => "ext",
        0x9123_683e => "btrfs",
        0x5846_5342 => "xfs",
        0x2fc1_2fc1 => "zfs",
        0x794c_7630 => "overlay",
        0x6969 => "nfs",
        0x6573_5546 => "fuse",
        0x9fa0 => "proc",
        0x6265_6572 => "sysfs",
        _ => return (None, FilesystemBacking::Unknown),
    }
    .to_owned();
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    let name = {
        let bytes: Vec<u8> = stat
            .f_fstypename
            .iter()
            .map(|byte| *byte as u8)
            .take_while(|byte| *byte != 0)
            .collect();
        match String::from_utf8(bytes) {
            Ok(name) if !name.is_empty() => name,
            _ => return (None, FilesystemBacking::Unknown),
        }
    };
    let backing = match name.as_str() {
        "tmpfs" | "ramfs" | "mfs" => FilesystemBacking::Memory,
        _ => FilesystemBacking::Other,
    };
    (Some(name), backing)
}

#[cfg(test)]
mod tests;
