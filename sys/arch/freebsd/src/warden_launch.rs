//! Explicit root-invoked development launcher. A private socketpair connects a
//! non-root harness to its single-threaded Warden parent. No public socket,
//! setuid installation, sudo invocation, or module load is performed.

use crate::warden_channel::Channel;
use crate::warden_client::CONTROL_ENV;
use crate::warden_process::CommandIdentity;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::time::Duration;

struct User {
    identity: CommandIdentity,
    uid: libc::uid_t,
    gid: libc::gid_t,
    groups: Vec<libc::gid_t>,
    name: CString,
    home: OsString,
    shell: OsString,
}

// FreeBSD16 sys/procdesc.h; resolve before fork so older userlands can still
// build/use the direct helper and fail closed only when Warden is requested.
type PdWait = unsafe extern "C" fn(
    libc::c_int,
    *mut libc::c_int,
    libc::c_int,
    *mut libc::c_void,
    *mut libc::c_void,
) -> libc::c_int;

fn resolve_pdwait() -> io::Result<PdWait> {
    let pointer = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"pdwait".as_ptr()) };
    if pointer.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Warden launcher requires FreeBSD16 process descriptors",
        ));
    }
    Ok(unsafe { std::mem::transmute::<*mut libc::c_void, PdWait>(pointer) })
}

/// Dedicated single-threaded root launcher, before any harness runtime exists.
pub fn launch(user: &str, argv: &[OsString]) -> io::Result<i32> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    // A restart alone does not make a missing/incorrect driver safe to use.
    crate::gulag::PreparedPolicy::new(chaos_ipc::protocol::SocketPolicy::Restricted)
        .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error.to_string()))?;
    launch_inner(lookup_user(user)?, argv)
}

fn launch_inner(user: User, argv: &[OsString]) -> io::Result<i32> {
    let pdwait = resolve_pdwait()?;
    if argv.is_empty() || argv.len() > 4096 || !Path::new(&argv[0]).is_absolute() {
        return Err(invalid(
            "Warden launcher requires an absolute harness executable",
        ));
    }
    let args = argv
        .iter()
        .map(|arg| cstring(arg))
        .collect::<io::Result<Vec<_>>>()?;
    let mut pointers: Vec<_> = args.iter().map(|arg| arg.as_ptr()).collect();
    pointers.push(std::ptr::null());
    let (client, worker) = Channel::pair()?;
    let client_fd = client.as_fd().as_raw_fd();
    let environment = environment(&user, client_fd)?;
    let mut envp: Vec<_> = environment.iter().map(|value| value.as_ptr()).collect();
    envp.push(std::ptr::null());
    let mut descriptor = -1;
    const PD_NOWAITPID: libc::c_int = 4;
    let pid = unsafe { libc::pdfork(&mut descriptor, libc::PD_CLOEXEC | PD_NOWAITPID) };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // Everything was allocated/resolved before pdfork. Do not return,
        // unwind, format messages, or touch the Rust runtime in this branch.
        unsafe {
            if libc::close_range(3, u32::MAX, libc::CLOSE_RANGE_CLOEXEC as _) != 0
                || libc::fcntl(client_fd, libc::F_SETFD, 0) != 0
                || libc::setgroups(user.groups.len() as _, user.groups.as_ptr()) != 0
                || libc::setresgid(user.gid, user.gid, user.gid) != 0
                || libc::setresuid(user.uid, user.uid, user.uid) != 0
            {
                libc::_exit(126);
            }
            libc::execve(args[0].as_ptr(), pointers.as_ptr(), envp.as_ptr());
            libc::_exit(126);
        }
    }
    let process = unsafe { OwnedFd::from_raw_fd(descriptor) };
    // The root broker alone owns the harness lifetime capability. Future
    // command forks must not keep it alive if the broker dies during setup.
    crate::warden_jail::protect_owner_descriptor(&process)?;
    drop(client);
    // A parent-terminal interrupt belongs to the non-root UI, not the root
    // broker. Other termination still closes owning jail/process descriptors.
    let previous_interrupt = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    let outcome = (|| {
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: process.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: worker.as_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if fds[0].revents != 0 {
                break;
            }
            if fds[1].revents != 0
                && !crate::warden_transport::serve_one(
                    &worker,
                    &user.identity,
                    Path::new("/tmp"),
                    Some(process.as_fd()),
                    Duration::from_secs(45 * 60),
                )?
            {
                // exit closes sockets before publishing process-descriptor
                // HUP. Allow that short, bounded drain rather than mistaking
                // ordinary harness exit for a lost authority capability.
                let mut exiting = libc::pollfd {
                    fd: process.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut exiting, 1, 5000) } > 0 {
                    break;
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "harness lost its Warden capability without exiting",
                ));
            }
        }
        let mut status = 0;
        loop {
            if unsafe {
                pdwait(
                    process.as_raw_fd(),
                    &mut status,
                    libc::WEXITED,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } >= 0
            {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        let status = std::process::ExitStatus::from_raw(status);
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
    })();
    // PD_NOWAITPID reaps on close; the process descriptor also prevents a
    // worker failure from leaving an unowned harness running.
    drop(process);
    unsafe {
        libc::signal(libc::SIGINT, previous_interrupt);
    }
    outcome
}

fn lookup_user(name: &str) -> io::Result<User> {
    let name = CString::new(name).map_err(|_| invalid("NUL in user name"))?;
    let mut storage = vec![0_u8; 64 * 1024];
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    let result = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            record.as_mut_ptr(),
            storage.as_mut_ptr().cast(),
            storage.len(),
            &mut found,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    if found.is_null() {
        return Err(invalid("unknown Warden launch user"));
    }
    let record = unsafe { record.assume_init() };
    let mut groups = vec![0; 1024];
    let mut count = groups.len() as i32;
    if unsafe {
        libc::getgrouplist(
            name.as_ptr(),
            record.pw_gid,
            groups.as_mut_ptr(),
            &mut count,
        )
    } < 0
        || count <= 0
        || count as usize > groups.len()
    {
        return Err(invalid("cannot obtain bounded launch-user groups"));
    }
    groups.truncate(count as usize);
    let identity = CommandIdentity::new(record.pw_uid, record.pw_gid, groups.clone())?;
    let home = OsString::from_vec(unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes().to_vec());
    let shell = OsString::from_vec(
        unsafe { CStr::from_ptr(record.pw_shell) }
            .to_bytes()
            .to_vec(),
    );
    Ok(User {
        identity,
        uid: record.pw_uid,
        gid: record.pw_gid,
        groups,
        name,
        home,
        shell,
    })
}

fn environment(user: &User, fd: i32) -> io::Result<Vec<CString>> {
    // Never copy root's provider tokens, loader settings or ambient authority
    // into the harness. Its normal HOME-based dotenv/config startup runs as UID.
    let mut entries = Vec::new();
    for (name, value) in [
        ("HOME", user.home.as_os_str()),
        ("SHELL", user.shell.as_os_str()),
        ("USER", OsStr::from_bytes(user.name.to_bytes())),
        ("LOGNAME", OsStr::from_bytes(user.name.to_bytes())),
        (
            "PATH",
            OsStr::new("/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin"),
        ),
    ] {
        entries.push(env_entry(name, value)?);
    }
    for name in ["TERM", "LANG", "LC_ALL", "LC_CTYPE"] {
        if let Some(value) = std::env::var_os(name) {
            entries.push(env_entry(name, &value)?);
        }
    }
    entries.push(env_entry(CONTROL_ENV, OsStr::new(&fd.to_string()))?);
    Ok(entries)
}

fn env_entry(name: &str, value: &OsStr) -> io::Result<CString> {
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(b'=');
    bytes.extend_from_slice(value.as_bytes());
    CString::new(bytes).map_err(|_| invalid("NUL in launch environment"))
}
fn cstring(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| invalid("NUL in launch argument"))
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub(crate) fn run_cli(args: Vec<OsString>) -> ! {
    let result = if args.len() >= 5
        && args[0] == "--warden-launch"
        && args[1] == "--user"
        && args[3] == "--"
    {
        match args[2].to_str() {
            Some(user) => launch(user, &args[4..]),
            None => Err(invalid("launch user must be UTF-8")),
        }
    } else {
        Err(invalid(
            "usage: alcatraz --warden-launch --user NAME -- /absolute/path/to/chaos [args]",
        ))
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Warden launcher: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_identity_is_nss_state_and_never_root() {
        assert!(lookup_user("root").is_err());
        assert!(lookup_user("chaos-intentionally-missing-user-9d4d").is_err());
        assert!(lookup_user("bad\0name").is_err());
    }

    #[test]
    #[ignore = "explicit root-only process-descriptor bootstrap test; no jail or MAC module"]
    fn native_launcher_drops_identity_and_returns_status_by_process_descriptor() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let user = lookup_user("nobody").unwrap();
        let argv = [
            "/bin/sh",
            "-c",
            "test \"$(/usr/bin/id -u)\" = 65534 && exit 29",
        ]
        .map(OsString::from);
        assert_eq!(launch_inner(user, &argv).unwrap(), 29);
    }
}
