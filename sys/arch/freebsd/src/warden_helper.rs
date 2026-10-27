//! RPC branch of the unprivileged Alcatraz helper. Never executes a target
//! locally after opting into Warden, including protocol/setup failure.

use crate::warden::CommandRequest;
use crate::warden_channel::Channel;
use crate::warden_transport::{self, Response};
use chaos_ipc::protocol::{SocketPolicy, VfsPolicy};
use std::collections::BTreeMap;
use std::io;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;

pub(crate) fn run(
    channel: Channel,
    filesystem: VfsPolicy,
    network: SocketPolicy,
    policy_cwd: PathBuf,
    mut argv: Vec<String>,
) -> ! {
    let result = (|| {
        let executable = crate::freebsd_run_main::resolve_executable_path(&argv[0])?;
        argv[0] = executable
            .into_os_string()
            .into_string()
            .map_err(|_| unsupported("non-UTF8 executable path"))?;
        let environment = std::env::vars_os()
            .map(|(name, value)| {
                let name = name
                    .into_string()
                    .map_err(|_| unsupported("non-UTF8 environment name"))?;
                let value = value
                    .into_string()
                    .map_err(|_| unsupported("non-UTF8 environment value"))?;
                Ok((name, value))
            })
            .collect::<io::Result<BTreeMap<_, _>>>()?;
        let command = CommandRequest {
            filesystem,
            network,
            argv,
            environment,
            cwd: std::env::current_dir()?,
            policy_cwd,
        };
        let stdio = unsafe {
            [
                BorrowedFd::borrow_raw(0),
                BorrowedFd::borrow_raw(1),
                BorrowedFd::borrow_raw(2),
            ]
        };
        warden_transport::submit(&channel, command, stdio)?.wait()
    })();
    drop(channel);
    match result {
        Ok(Response::Exited {
            code: Some(code),
            signal: None,
        }) => std::process::exit(code),
        Ok(Response::Exited {
            code: None,
            signal: Some(signal),
        }) => exit_with_signal(signal),
        Ok(Response::Error { message }) => {
            eprintln!("alcatraz: Warden: {message}");
            std::process::exit(125);
        }
        Err(error) => {
            eprintln!("alcatraz: Warden: {error}");
            std::process::exit(125);
        }
        _ => std::process::exit(125),
    }
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

fn exit_with_signal(signal: i32) -> ! {
    unsafe {
        // Preserve signal status without manufacturing a second core dump of
        // the RPC helper. The target's own dump behavior belongs to its jail.
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::setrlimit(libc::RLIMIT_CORE, &limit);
        libc::signal(signal, libc::SIG_DFL);
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, signal);
        libc::sigprocmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut());
        libc::kill(libc::getpid(), signal);
        libc::_exit(128 + signal);
    }
}
