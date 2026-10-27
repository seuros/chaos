//! Child setup for the single-threaded privileged Warden worker.
//!
//! This is a low-level primitive, not the public sandbox dispatch path. It
//! does not compile/verify a filesystem policy. It applies the ABI-checked MAC
//! network/process label after attachment, identity dropping and hardening.
//! Keep the owning jail handle until the process tree is to be removed.

use crate::gulag::PreparedPolicy;
use crate::warden_jail::OwnedJail;
use chaos_ipc::protocol::SocketPolicy;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

/// Identity supplied by the authenticated Warden launcher, NEVER by an
/// untrusted command/request body. No root command identities are supported.
#[derive(Debug, Clone)]
pub struct CommandIdentity {
    uid: libc::uid_t,
    gid: libc::gid_t,
    groups: Vec<libc::gid_t>,
}

impl CommandIdentity {
    pub fn new(uid: libc::uid_t, gid: libc::gid_t, groups: Vec<libc::gid_t>) -> io::Result<Self> {
        if uid == 0
            || uid == u32::MAX
            || gid == u32::MAX
            || groups.len() > 1024
            || groups.contains(&u32::MAX)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid non-root command identity",
            ));
        }
        Ok(Self { uid, gid, groups })
    }
}

/// Consume the command so the parent's temporary attachment authority is
/// closed immediately after spawn, not retained in a reusable Command.
///
/// Must be called in a dedicated single-threaded Warden worker. This
/// marks all non-stdio parent descriptors CLOEXEC; no unrelated concurrent
/// exec/fork or library users may depend on their previous inheritance flags.
pub fn spawn(
    jail: &OwnedJail,
    command: Command,
    identity: &CommandIdentity,
    cwd: &Path,
    network: SocketPolicy,
) -> io::Result<Child> {
    let policy = PreparedPolicy::new(network)
        .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error.to_string()))?;
    spawn_inner(jail, command, identity, cwd, Some(policy))
}

pub(crate) fn spawn_inner(
    jail: &OwnedJail,
    mut command: Command,
    identity: &CommandIdentity,
    cwd: &Path,
    mut policy: Option<PreparedPolicy>,
) -> io::Result<Child> {
    if !Path::new(command.get_program()).is_absolute() || !cwd.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "jailed executable and cwd must be absolute",
        ));
    }
    let cwd = CString::new(cwd.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let attachment = jail.attachment()?;
    let inherited = inherited_descriptors(attachment.raw_descriptor())?;
    let identity = identity.clone();
    let group_count = libc::c_int::try_from(identity.groups.len())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // Do not close std::Command's future exec-error pipe in pre_exec.
    // Instead all inherited authority is CLOEXEC before fork, while the
    // known attachment descriptor is explicitly consumed in trusted setup.
    if unsafe { libc::close_range(3, u32::MAX, libc::CLOSE_RANGE_CLOEXEC as libc::c_int) } != 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        command.pre_exec(move || {
            // Only direct syscalls / raw-errno Results below. All strings,
            // vectors and symbol resolution were prepared before fork.
            // Never join Warden's own process group or controlling terminal.
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            for signal in [
                libc::SIGINT,
                libc::SIGTERM,
                libc::SIGHUP,
                libc::SIGQUIT,
                libc::SIGCHLD,
            ] {
                if libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(io::Error::last_os_error());
                }
            }
            // CLOEXEC alone is insufficient: jail attachment refuses open
            // directory descriptors, even when they would close at exec.
            // These numbers were occupied before Command created its error
            // pipe, so closing them cannot close that future pipe.
            for &fd in &inherited {
                if libc::close(fd) != 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EBADF) {
                        return Err(error);
                    }
                }
            }
            if libc::setgroups(group_count, identity.groups.as_ptr()) != 0
                || libc::setresgid(identity.gid, identity.gid, identity.gid) != 0
                || libc::setresuid(identity.uid, identity.uid, identity.uid) != 0
            {
                return Err(io::Error::last_os_error());
            }
            // Drop host identity first: allow.nosuser intentionally prevents
            // relying on root privilege after attachment. The stored root
            // credentials of the scoped descriptor authorize only the attach.
            attachment.attach_in_child()?;
            if libc::chdir(cwd.as_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut flag: libc::c_int = libc::PROC_NO_NEW_PRIVS_ENABLE;
            if libc::procctl(
                libc::P_PID,
                0,
                libc::PROC_NO_NEW_PRIVS_CTL,
                std::ptr::addr_of_mut!(flag).cast(),
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            flag = libc::PROC_TRACE_CTL_DISABLE_EXEC;
            if libc::procctl(
                libc::P_PID,
                0,
                libc::PROC_TRACE_CTL,
                std::ptr::addr_of_mut!(flag).cast(),
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            if let Some(policy) = policy.as_mut() {
                policy.apply_in_child()?;
            }
            Ok(())
        });
    }
    command.spawn()
}

fn inherited_descriptors(attachment: libc::c_int) -> io::Result<Vec<libc::c_int>> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_FILEDESC,
        unsafe { libc::getpid() },
    ];
    for _ in 0..4 {
        let mut size = 0;
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if size > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "descriptor snapshot too large",
            ));
        }
        // sysctl's size query is a hint, not a reservation. Leave bounded
        // headroom for libc/NSS or test-runner descriptor changes between
        // calls; the returned packed length is still checked by the parser.
        size = size
            .saturating_add(16 * std::mem::size_of::<libc::kinfo_file>())
            .min(16 * 1024 * 1024);
        let mut bytes = vec![0_u8; size];
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null(),
                0,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOMEM) {
                continue;
            }
            return Err(error);
        }
        bytes.truncate(size);
        return parse_descriptors(&bytes, attachment);
    }
    Err(io::Error::other("descriptor table changed during snapshot"))
}

fn parse_descriptors(mut bytes: &[u8], attachment: libc::c_int) -> io::Result<Vec<libc::c_int>> {
    let mut descriptors = Vec::new();
    let offset = std::mem::offset_of!(libc::kinfo_file, kf_fd);
    while !bytes.is_empty() {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid descriptor snapshot");
        let len = i32::from_ne_bytes(
            bytes
                .get(..4)
                .ok_or_else(invalid)?
                .try_into()
                .map_err(|_| invalid())?,
        );
        let len = usize::try_from(len).map_err(|_| invalid())?;
        if len < offset + 4 || len > bytes.len() {
            return Err(invalid());
        }
        let fd = i32::from_ne_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| invalid())?,
        );
        if fd > 2 && fd != attachment {
            descriptors.push(fd);
        }
        bytes = &bytes[len..];
    }
    descriptors.sort_unstable();
    descriptors.dedup();
    Ok(descriptors)
}

#[cfg(test)]
mod tests;
