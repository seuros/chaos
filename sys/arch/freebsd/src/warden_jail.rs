//! Jail resource ownership for Warden, the privileged userspace manager.
//!
//! FreeBSD 16 owning jail descriptors bind teardown to a prison object,
//! avoiding numeric-JID reuse races. Closing the owner begins jail removal,
//! including when Warden is killed. Mount cleanup still requires a
//! journal/recovery path and waiting for descendants to drain.
//!
//! This primitive NEVER attaches the calling process or executes a command.
//! It verifies only the root mount, not a complete filesystem policy. It is
//! not wired into the direct helper as a way to bypass VFS restrictions.

use chaos_ipc::protocol::SocketPolicy;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

// sys/jail.h in FreeBSD 16. Older kernels must reject this unknown flag; never
// retry without it and create a jail whose lifetime cannot be safely owned.
const JAIL_OWN_DESC: libc::c_int = 0x80;
// sys/fcntl.h; not yet exposed by the workspace's libc crate.
const FD_CLOFORK: libc::c_int = 4;
const CREATE_FLAGS: libc::c_int = libc::JAIL_CREATE | JAIL_OWN_DESC;

/// Pins a private, root-owned, read-only jail-root mount. This verifies neither
/// writable overlays nor devices; Warden must verify those separately.
#[derive(Debug)]
pub struct ReadOnlyRootMount {
    path: PathBuf,
    directory: File,
}

impl ReadOnlyRootMount {
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("jail root must be absolute"));
        }
        if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(invalid("jail root must not be a symlink"));
        }
        let path = std::fs::canonicalize(path)?;
        if path == Path::new("/") {
            return Err(invalid("the host root is not a private jail-root mount"));
        }
        validate_root_ancestry(&path)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&path)?;
        let metadata = directory.metadata()?;
        let named = std::fs::symlink_metadata(&path)?;
        if metadata.dev() != named.dev() || metadata.ino() != named.ino() {
            return Err(invalid("jail root changed while opening it"));
        }
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: live descriptor, correctly sized writable statfs output.
        if unsafe { libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatfs succeeded and initialized the output.
        let stat = unsafe { stat.assume_init() };
        if stat.f_flags & libc::MNT_RDONLY as u64 == 0 {
            return Err(invalid("jail root must be mounted read-only"));
        }
        let mount_path: Vec<u8> = stat
            .f_mntonname
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        if Path::new(OsStr::from_bytes(&mount_path)) != path {
            return Err(invalid(
                "jail root must be the mount root, not a subdirectory",
            ));
        }
        Ok(Self { path, directory })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn validate_root_ancestry(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || metadata.uid() != 0 {
            return Err(invalid(
                "jail root and its ancestors must be root-owned directories",
            ));
        }
        let writable = metadata.mode() & 0o022 != 0;
        // A sticky root-owned parent such as /tmp protects the root-owned
        // child entry. The jail root itself may never be group/world writable.
        if writable && (ancestor == path || metadata.mode() & 0o1000 == 0) {
            return Err(invalid("untrusted writable ancestor of jail root"));
        }
    }
    Ok(())
}

/// Privileged Warden handle. Do not duplicate, pass over IPC, or expose its
/// descriptor to commands: jail descriptors carry authority, not just identity.
///
/// Creation belongs in a single-threaded setup/exec helper: the kernel does
/// not atomically set CLOEXEC/CLOFORK on the returned descriptor, so no concurrent
/// fork/exec may run between jail_set and the immediate fcntl below.
#[must_use = "dropping the owning descriptor begins removal of its jail"]
#[derive(Debug)]
pub struct OwnedJail {
    // Drop order matters: request jail removal before releasing the root pin.
    owner: OwnedFd,
    root: ReadOnlyRootMount,
    diagnostic_jid: libc::c_int,
    name: String,
}

impl OwnedJail {
    /// Creates only an empty persistent jail. Does not attach, change identity,
    /// mount anything, or establish that a command's filesystem policy is met.
    /// Call only after the complete view has been provisioned and verified.
    pub fn create(root: ReadOnlyRootMount, name: &str, network: SocketPolicy) -> io::Result<Self> {
        // SAFETY: geteuid has no pointer arguments or side effects.
        if unsafe { libc::geteuid() } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "jail setup requires root",
            ));
        }
        // Keep the pin live until after jail creation (and through teardown).
        let _ = root.directory.metadata()?;
        let mut options = JailOptions::new(root.path(), name, network)?;
        let jid = {
            let mut iov = options.iovecs();
            let count = u32::try_from(iov.len()).map_err(|_| invalid("too many jail options"))?;
            // SAFETY: all iovec names and mutable values are owned by options,
            // remain live and stable for this synchronous call, and are sized.
            unsafe { libc::jail_set(iov.as_mut_ptr(), count, CREATE_FLAGS) }
        };
        if jid < 0 {
            return Err(io::Error::last_os_error());
        }
        let descriptor = options.returned_descriptor()?;
        // SAFETY: a successful jail_set with JAIL_OWN_DESC returns a fresh
        // owning descriptor through the desc output, uniquely owned here.
        let owner = unsafe { OwnedFd::from_raw_fd(descriptor) };
        protect_owner_descriptor(&owner)?;
        Ok(Self {
            owner,
            root,
            diagnostic_jid: jid,
            name: name.to_owned(),
        })
    }

    /// For logging only. Cleanup MUST NOT use this numeric ID.
    pub fn diagnostic_jid(&self) -> libc::c_int {
        self.diagnostic_jid
    }

    /// Begin removal through the owning descriptor and return the root pin.
    /// Removal is not proof all processes have exited or mounts are idle:
    /// Warden must wait before unmounting and report cleanup errors.
    pub fn begin_remove(self) -> ReadOnlyRootMount {
        drop(self.owner);
        self.root
    }

    /// Prepare a nonowning authority descriptor for one trusted child setup.
    /// It must be closed before executing the requested command.
    pub fn attachment(&self) -> io::Result<JailAttachment> {
        let attach = resolve_attach()?;
        let mut output = -1_i32;
        let name = CString::new(self.name.as_str()).map_err(|_| invalid("NUL in jail name"))?;
        let mut iov = [
            libc::iovec {
                iov_base: c"name".as_ptr().cast_mut().cast(),
                iov_len: 5,
            },
            libc::iovec {
                iov_base: name.as_ptr().cast_mut().cast(),
                iov_len: name.as_bytes_with_nul().len(),
            },
            libc::iovec {
                iov_base: c"desc".as_ptr().cast_mut().cast(),
                iov_len: 5,
            },
            libc::iovec {
                iov_base: std::ptr::addr_of_mut!(output).cast(),
                iov_len: std::mem::size_of_val(&output),
            },
        ];
        const JAIL_GET_DESC: libc::c_int = 0x40;
        let copied_jid =
            unsafe { libc::jail_get(iov.as_mut_ptr(), iov.len() as u32, JAIL_GET_DESC) };
        if copied_jid < 0 {
            return Err(io::Error::last_os_error());
        }
        if output < 0 {
            return Err(invalid("no attachment descriptor returned"));
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(output) };
        // Do not combine GET_DESC and USE_DESC: unpatched 16-CURRENT skips
        // descriptor allocation in that branch. Validate the name lookup
        // against our original *live object* before using the copy.
        let mut original = self.owner.as_raw_fd();
        let mut query = [
            libc::iovec {
                iov_base: c"desc".as_ptr().cast_mut().cast(),
                iov_len: 5,
            },
            libc::iovec {
                iov_base: std::ptr::addr_of_mut!(original).cast(),
                iov_len: std::mem::size_of_val(&original),
            },
        ];
        const JAIL_USE_DESC: libc::c_int = 0x10;
        let live_jid =
            unsafe { libc::jail_get(query.as_mut_ptr(), query.len() as u32, JAIL_USE_DESC) };
        if live_jid < 0 {
            return Err(io::Error::last_os_error());
        }
        if live_jid != copied_jid || live_jid != self.diagnostic_jid {
            return Err(invalid("jail changed during attachment preparation"));
        }
        // This nonowning copy intentionally survives fork, but never exec.
        if unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(JailAttachment { descriptor, attach })
    }
}

type Attach = unsafe extern "C" fn(libc::c_int) -> libc::c_int;

/// Authority for trusted pre-exec setup, not a command capability.
pub struct JailAttachment {
    descriptor: OwnedFd,
    attach: Attach,
}

impl JailAttachment {
    pub(crate) fn raw_descriptor(&self) -> libc::c_int {
        self.descriptor.as_raw_fd()
    }

    /// # Safety
    /// Call only in the single-threaded child setup path. Success changes its
    /// root/cwd/prison. No untrusted code may run with the authority FD open.
    /// The child must immediately exec or _exit after setup; it must not unwind
    /// or return to a parent runtime after manually closing its copy of the FD.
    pub(crate) unsafe fn attach_in_child(&self) -> io::Result<()> {
        if unsafe { (self.attach)(self.descriptor.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::close(self.descriptor.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn resolve_attach() -> io::Result<Attach> {
    let function = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"jail_attach_jd".as_ptr()) };
    if function.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "descriptor-based jail attachment requires FreeBSD 16",
        ));
    }
    // The symbol's exact C prototype is int jail_attach_jd(int), sys/jail.h.
    Ok(unsafe { std::mem::transmute::<*mut libc::c_void, Attach>(function) })
}

pub(crate) fn protect_owner_descriptor(fd: &OwnedFd) -> io::Result<()> {
    // Preserve any existing descriptor flags.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: borrowed descriptor is valid and remains owned through the call.
    if unsafe {
        libc::fcntl(
            fd.as_raw_fd(),
            libc::F_SETFD,
            flags | libc::FD_CLOEXEC | FD_CLOFORK,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct JailOptions {
    values: Vec<(&'static CStr, Vec<u8>)>,
    descriptor_index: usize,
}

impl JailOptions {
    fn new(path: &Path, name: &str, network: SocketPolicy) -> io::Result<Self> {
        if !name.starts_with("chaos-")
            || name.len() > 63
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(invalid(
                "jail names must be bounded, flat chaos-* identifiers",
            ));
        }
        let path =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("NUL in jail path"))?;
        let name = CString::new(name).map_err(|_| invalid("NUL in jail name"))?;
        let mut options = Self {
            values: Vec::new(),
            descriptor_index: 0,
        };
        options.push(c"name", name.into_bytes_with_nul());
        options.push(c"path", path.into_bytes_with_nul());
        options.push(c"persist", Vec::new());
        options.descriptor_index = options.values.len();
        options.integer(c"desc", -1);
        options.integer(c"children.max", 0);
        options.integer(c"enforce_statfs", 2);
        let ip_mode = if network.is_enabled() {
            libc::JAIL_SYS_INHERIT
        } else {
            libc::JAIL_SYS_DISABLE
        };
        options.integer(c"ip4", ip_mode);
        options.integer(c"ip6", ip_mode);
        for name in [
            c"allow.nomount",
            c"allow.noraw_sockets",
            c"allow.nosysvipc",
            c"allow.nochflags",
            c"allow.noset_hostname",
            c"allow.nosocket_af",
            c"allow.nosuser",
        ] {
            options.push(name, Vec::new());
        }
        Ok(options)
    }

    fn push(&mut self, name: &'static CStr, value: Vec<u8>) {
        self.values.push((name, value));
    }

    fn integer(&mut self, name: &'static CStr, value: i32) {
        self.push(name, value.to_ne_bytes().to_vec());
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

    fn returned_descriptor(&self) -> io::Result<i32> {
        let bytes: [u8; 4] = self.values[self.descriptor_index]
            .1
            .as_slice()
            .try_into()
            .map_err(|_| invalid("invalid jail descriptor output length"))?;
        let fd = i32::from_ne_bytes(bytes);
        if fd < 0 {
            return Err(invalid("kernel did not return an owning jail descriptor"));
        }
        Ok(fd)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
