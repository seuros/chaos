use super::{MountId, MountRecord, devices, journal, rollback};
use crate::warden_jail::validate_root_ancestry;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

#[must_use = "mounts must be explicitly cleaned up after jail/process removal"]
pub struct ScopedMounts {
    root: PathBuf,
    journal: journal::Journal,
    owned: Vec<MountRecord>,
    uncertain: bool,
    cleaned: bool,
}

impl ScopedMounts {
    /// Allocate a root-owned, private state directory and a descriptor-only
    /// control mount. No host mount, ruleset or configuration is modified.
    pub fn create(parent: &Path) -> io::Result<Self> {
        root_only()?;
        let parent = std::fs::canonicalize(parent)?;
        let mut bytes = [0_u8; 16];
        if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let root = parent.join(format!("chaos-jail-{suffix}"));
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        // No mount exists until the journal and scope are fully initialized.
        let initialize = || -> io::Result<journal::Journal> {
            validate_root_ancestry(&root)?;
            for dir in ["fd", "view"] {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(root.join(dir))?;
            }
            journal::Journal::create(&root)
        };
        let journal = match initialize() {
            Ok(journal) => journal,
            Err(error) => {
                let _ = std::fs::remove_file(root.join("mounts.jsonl"));
                let _ = std::fs::remove_dir(root.join("fd"));
                let _ = std::fs::remove_dir(root.join("view"));
                let _ = std::fs::remove_dir(&root);
                return Err(error);
            }
        };
        let mut scope = Self {
            root,
            journal,
            owned: Vec::new(),
            uncertain: false,
            cleaned: false,
        };
        let target = scope.root.join("fd");
        let mut options = Options::new(c"fdescfs", &target)?;
        options.flag(c"nodup");
        scope.perform(options, &target, true, true)?;
        Ok(scope)
    }

    pub fn state_path(&self) -> &Path {
        &self.root
    }
    pub fn view_path(&self) -> PathBuf {
        self.root.join("view")
    }
    pub fn records(&self) -> &[MountRecord] {
        &self.owned
    }

    /// Directory mounts only in this implementation. `relative` is beneath
    /// the view root and contains no symlink traversal or parent components.
    pub fn nullfs(&mut self, source: &Path, relative: &Path, read_only: bool) -> io::Result<()> {
        self.nullfs_checked(source, relative, read_only, None)
    }

    pub(crate) fn nullfs_checked(
        &mut self,
        source: &Path,
        relative: &Path,
        read_only: bool,
        expected_source: Option<MountId>,
    ) -> io::Result<()> {
        let source = pin_absolute_directory(source)?;
        if let Some(expected) = expected_source
            && stat_file(&source)?.id != expected
        {
            return Err(invalid("host mount changed while pinning source"));
        }
        let (target, target_file) = self.target(relative)?;
        let from = self.root.join("fd").join(source.as_raw_fd().to_string());
        let destination = self
            .root
            .join("fd")
            .join(target_file.as_raw_fd().to_string());
        let mut options = Options::new(c"nullfs", &destination)?;
        options.path(c"from", &from)?;
        options.string(c"check_fsid", &stat_file(&target_file)?.id.kernel_name())?;
        self.perform(options, &target, read_only, false)
    }

    /// A private empty read-only filesystem masks unsupported synthetic
    /// mounts. It is NOT a substitute for the contents of a read-only carveout.
    pub fn empty_mask(&mut self, relative: &Path) -> io::Result<()> {
        let (target, target_file) = self.target(relative)?;
        let destination = self
            .root
            .join("fd")
            .join(target_file.as_raw_fd().to_string());
        let mut options = Options::new(c"tmpfs", &destination)?;
        options.string(c"size", "1048576")?;
        options.string(c"mode", "0555")?;
        options.string(c"check_fsid", &stat_file(&target_file)?.id.kernel_name())?;
        self.perform(options, &target, true, true)
    }

    /// Fresh per-view devfs, with no host devices other than null/zero/random.
    /// Requires an existing, exactly hide-only global ruleset 1; never creates
    /// or edits rulesets. No terminal, disk, zfs, fuse, or host fdesc exposure.
    pub fn minimal_devices(&mut self, relative: &Path) -> io::Result<()> {
        let host_devices = File::open("/dev")?;
        devices::verify_hide_only(&host_devices)?;
        let (target, target_file) = self.target(relative)?;
        let destination = self
            .root
            .join("fd")
            .join(target_file.as_raw_fd().to_string());
        let mut options = Options::new(c"devfs", &destination)?;
        options.string(c"ruleset", devices::RULESET)?;
        options.string(c"check_fsid", &stat_file(&target_file)?.id.kernel_name())?;
        self.perform(options, &target, true, true)?;
        let (_, directory) = self.target(relative)?;
        if stat_file(&directory)?.filesystem_type != "devfs" {
            return Err(invalid("device target was replaced"));
        }
        devices::configure(&directory, &target)
    }

    fn target(&self, relative: &Path) -> io::Result<(PathBuf, File)> {
        if self.uncertain || self.cleaned {
            return Err(invalid("mount scope is not usable"));
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(self.view_path())?;
        let directory = pin_relative_directory(directory, relative)?;
        Ok((self.view_path().join(relative), directory))
    }

    fn perform(
        &mut self,
        mut options: Options,
        expected: &Path,
        read_only: bool,
        no_exec: bool,
    ) -> io::Result<()> {
        if self.uncertain || self.cleaned {
            return Err(invalid("mount scope is not usable"));
        }
        let before = snapshot()?;
        let kind = options.kind.clone();
        self.journal.append(&journal::Event::Before {
            filesystem_type: kind.clone(),
            target: expected.to_owned(),
            prior_ids: before.iter().map(|m| m.id).collect(),
        })?;
        let mut flags = libc::MNT_NOSUID;
        if read_only {
            flags |= libc::MNT_RDONLY;
        }
        if no_exec {
            flags |= libc::MNT_NOEXEC;
        }
        let mut iov = options.iovecs();
        let count = u32::try_from(iov.len()).map_err(|_| invalid("too many mount options"))?;
        if unsafe { libc::nmount(iov.as_mut_ptr(), count, flags) } != 0 {
            let error = io::Error::last_os_error();
            self.journal.append(&journal::Event::MountFailed)?;
            return Err(error);
        }
        // A synced Before record permits recovery even if the process dies
        // here, before the new ID can be appended.
        self.uncertain = true;
        let after = snapshot()?;
        let mut added: Vec<_> = after
            .into_iter()
            .filter(|m| {
                m.target.starts_with(&self.root) && !before.iter().any(|old| old.id == m.id)
            })
            .collect();
        if added.len() != 1 || added[0].filesystem_type != kind {
            return Err(invalid(
                "cannot identify the newly created scoped mount; recovery required",
            ));
        }
        let record = added.remove(0);
        self.owned.push(record.clone());
        self.uncertain = false;
        self.journal.append(&journal::Event::Mounted {
            record: record.clone(),
        })?;
        // fdesc_statfs deliberately reports only MNT_IGNORE, not its other
        // mount flags. This exception is for the private control mount,
        // which is protected by the root-only state directory and is never
        // part of a command's filesystem view. devfs_statfs also omits flags;
        // minimal_devices instead verifies its exact device-only namespace,
        // root ownership and persistent hide policy before any child exists.
        // Ordinary filesystem view mounts must report all requested flags.
        let private_control = kind == "fdescfs" && expected == self.root.join("fd");
        let verified_devices = kind == "devfs";
        if record.target != expected
            || (!(private_control || verified_devices)
                && (record.flags & libc::MNT_NOSUID as u64 == 0
                    || (read_only && record.flags & libc::MNT_RDONLY as u64 == 0)
                    || (no_exec && record.flags & libc::MNT_NOEXEC as u64 == 0)))
        {
            return Err(invalid(
                "mounted view did not match the requested location/flags",
            ));
        }
        Ok(())
    }

    /// Call only after jail removal and dropping all pins into owned mounts.
    /// EBUSY stops cleanup; it never triggers FORCE or removal of a parent.
    pub fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        if self.uncertain {
            return Err(invalid("uncertain mount state: use explicit recovery"));
        }
        let root = &self.root;
        let journal = &mut self.journal;
        let mut journal_error = None;
        rollback(&mut self.owned, |record| {
            unmount_owned(root, record)?;
            // Cleanup can still remove known-owned mounts when storage fails.
            // Keep the journal for recovery/reporting, but never append after
            // its first failed write.
            if journal_error.is_none()
                && let Err(error) = journal.append(&journal::Event::Unmounted { id: record.id })
            {
                journal_error = Some(error);
            }
            Ok(())
        })?;
        if let Some(error) = journal_error {
            return Err(error);
        }
        remove_state(&self.root)?;
        self.cleaned = true;
        Ok(())
    }

    /// Replays a trusted root-owned journal. A crash between nmount and the
    /// Mounted append is reconciled using the pre-syscall ID snapshot.
    pub fn recover(root: &Path) -> io::Result<()> {
        root_only()?;
        let root = std::fs::canonicalize(root)?;
        let root = root.as_path();
        validate_root_ancestry(root)?;
        let mut state = journal::read(root)?;
        if let Some((kind, prior, target)) = state.pending {
            let candidates: Vec<_> = snapshot()?
                .into_iter()
                .filter(|m| m.target.starts_with(root) && !prior.contains(&m.id))
                .collect();
            if candidates.len() > 1
                || candidates
                    .iter()
                    .any(|m| m.filesystem_type != kind || m.target != target)
            {
                return Err(invalid(
                    "ambiguous interrupted mount; refusing automatic cleanup",
                ));
            }
            state.owned.extend(candidates);
        }
        rollback(&mut state.owned, |record| unmount_owned(root, record))?;
        remove_state(root)
    }

    #[cfg(test)]
    pub(super) fn leave_for_recovery(mut self) -> PathBuf {
        self.cleaned = true;
        self.root.clone()
    }
}

/// Never reopen a caller-controlled absolute pathname with protection for only
/// its final component. Each successful openat pins the next real directory;
/// subsequent renames cannot redirect traversal through a replacement symlink.
pub(super) fn pin_absolute_directory(path: &Path) -> io::Result<File> {
    let relative = path
        .strip_prefix("/")
        .map_err(|_| invalid("source must be absolute"))?;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")?;
    pin_relative_directory(root, relative)
}

pub(super) fn pin_relative_directory(mut directory: File, relative: &Path) -> io::Result<File> {
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid(
                "directory path must be relative with no parent components",
            ));
        };
        let name = CString::new(name.as_bytes()).map_err(|_| invalid("NUL in directory path"))?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

impl Drop for ScopedMounts {
    fn drop(&mut self) {
        if !self.cleaned
            && let Err(error) = self.cleanup()
        {
            eprintln!(
                "alcatraz: scoped mount cleanup retained {}: {error}",
                self.root.display()
            );
        }
    }
}

fn remove_state(root: &Path) -> io::Result<()> {
    // Never recursively delete a mount tree or unknown files.
    for dir in ["view", "fd"] {
        match std::fs::remove_dir(root.join(dir)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    std::fs::remove_file(root.join("mounts.jsonl"))?;
    std::fs::remove_dir(root)
}

fn unmount_owned(root: &Path, record: &MountRecord) -> io::Result<()> {
    if !journal::scoped_path(root, &record.target) {
        return Err(invalid("mount escaped ownership scope"));
    }
    let current = snapshot()?.into_iter().find(|m| m.id == record.id);
    let Some(current) = current else {
        return Ok(());
    };
    if current.target != record.target
        || current.filesystem_type != record.filesystem_type
        || current.mounted_from != record.mounted_from
    {
        return Err(invalid("mount identity changed; refusing to unmount it"));
    }
    let id = CString::new(record.id.kernel_name()).map_err(|_| invalid("invalid mount ID"))?;
    if unsafe { libc::unmount(id.as_ptr(), libc::MNT_BYFSID) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn snapshot() -> io::Result<Vec<MountRecord>> {
    for _ in 0..6 {
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        let capacity = usize::try_from(count).map_err(|_| invalid("invalid mount count"))? + 16;
        if capacity > 16384 {
            return Err(invalid("mount table too large"));
        }
        let mut entries: Vec<std::mem::MaybeUninit<libc::statfs>> = Vec::with_capacity(capacity);
        // MaybeUninit elements may be uninitialized; the kernel fills the
        // returned count, and no other element is inspected.
        unsafe {
            entries.set_len(capacity);
        }
        let bytes = libc::c_long::try_from(capacity * std::mem::size_of::<libc::statfs>())
            .map_err(|_| invalid("mount snapshot overflow"))?;
        let found =
            unsafe { libc::getfsstat(entries.as_mut_ptr().cast(), bytes, libc::MNT_NOWAIT) };
        if found < 0 {
            return Err(io::Error::last_os_error());
        }
        let found = found as usize;
        if found >= capacity {
            continue;
        }
        return Ok(entries
            .into_iter()
            .take(found)
            .map(|entry| {
                let stat = unsafe { entry.assume_init() };
                mount_info(&stat)
            })
            .collect());
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "mount table changed repeatedly",
    ))
}

fn mount_info(stat: &libc::statfs) -> MountRecord {
    const {
        assert!(std::mem::size_of::<libc::fsid_t>() == std::mem::size_of::<[i32; 2]>());
    }
    // libc keeps fsid_t's two C int fields private. FreeBSD's fsid_t ABI is
    // exactly int32_t val[2]; size asserted above, no reference alias is made.
    let id =
        unsafe { std::ptr::read_unaligned(std::ptr::addr_of!(stat.f_fsid).cast::<[i32; 2]>()) };
    MountRecord {
        id: MountId(id),
        target: chars_path(&stat.f_mntonname),
        mounted_from: chars_path(&stat.f_mntfromname),
        filesystem_type: String::from_utf8_lossy(&chars_bytes(&stat.f_fstypename)).into_owned(),
        flags: stat.f_flags,
    }
}

fn chars_bytes(chars: &[libc::c_char]) -> Vec<u8> {
    chars
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as u8)
        .collect()
}
fn chars_path(chars: &[libc::c_char]) -> PathBuf {
    OsString::from_vec(chars_bytes(chars)).into()
}
fn stat_file(file: &File) -> io::Result<MountRecord> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mount_info(&unsafe { stat.assume_init() }))
}
fn root_only() -> io::Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "mount setup requires root",
        ))
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

struct Options {
    kind: String,
    values: Vec<(&'static CStr, Vec<u8>)>,
}
impl Options {
    fn new(kind: &'static CStr, target: &Path) -> io::Result<Self> {
        let mut options = Self {
            kind: kind.to_string_lossy().into_owned(),
            values: Vec::new(),
        };
        options
            .values
            .push((c"fstype", kind.to_bytes_with_nul().to_vec()));
        options.path(c"fspath", target)?;
        Ok(options)
    }
    fn flag(&mut self, name: &'static CStr) {
        self.values.push((name, Vec::new()));
    }
    fn path(&mut self, name: &'static CStr, value: &Path) -> io::Result<()> {
        self.values.push((
            name,
            CString::new(value.as_os_str().as_bytes())
                .map_err(|_| invalid("NUL in mount option"))?
                .into_bytes_with_nul(),
        ));
        Ok(())
    }
    fn string(&mut self, name: &'static CStr, value: &str) -> io::Result<()> {
        self.path(name, Path::new(OsStr::new(value)))
    }
    fn iovecs(&mut self) -> Vec<libc::iovec> {
        let mut iov = Vec::with_capacity(self.values.len() * 2);
        for (name, value) in &mut self.values {
            iov.push(libc::iovec {
                iov_base: name.as_ptr().cast_mut().cast(),
                iov_len: name.to_bytes_with_nul().len(),
            });
            iov.push(libc::iovec {
                iov_base: if value.is_empty() {
                    std::ptr::null_mut()
                } else {
                    value.as_mut_ptr().cast()
                },
                iov_len: value.len(),
            });
        }
        iov
    }
}
