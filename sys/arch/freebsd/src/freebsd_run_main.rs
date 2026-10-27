use std::ffi::CString;
use std::ffi::OsStr;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;

use crate::capsicum::apply_sandbox_policy_to_current_thread;
use alcatraz_base::sandbox_policy::{EffectiveSandboxPolicies, resolve_sandbox_policies};
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SocketPolicy;
use chaos_ipc::protocol::VfsPolicy;

#[derive(Debug, usage::Cli)]
#[usage(bin = "alcatraz", unknown_flags = "error", args_override_self = false)]
/// CLI surface for the FreeBSD sandbox helper.
///
/// Validates which sandbox policy combinations can be enforced safely on
/// FreeBSD, then execs the target command. Mirrors the Linux Alcatraz
/// interface for consistent cross-platform sandbox invocation.
pub struct CapsicumCommand {
    /// It is possible that the cwd used in the context of the sandbox policy
    /// is different from the cwd of the process to spawn.
    #[usage(long = "sandbox-policy-cwd")]
    pub sandbox_policy_cwd: PathBuf,

    /// Legacy compatibility policy.
    ///
    /// Newer callers pass split filesystem/network policies as well so the
    /// helper can migrate incrementally without breaking older invocations.
    #[usage(long = "sandbox-policy", hide)]
    pub sandbox_policy: Option<SandboxPolicy>,

    #[usage(long = "file-system-sandbox-policy", hide)]
    pub vfs_policy: Option<VfsPolicy>,

    #[usage(long = "network-sandbox-policy", hide)]
    pub socket_policy: Option<SocketPolicy>,

    /// Internal compatibility flag.
    ///
    /// Managed-network sessions request proxy-only connectivity through this
    /// flag. The current FreeBSD helper rejects that mode explicitly because
    /// proxy-routed networking is not implemented yet.
    #[usage(long = "allow-network-for-proxy", hide)]
    pub allow_network_for_proxy: bool,

    /// Full command args to run under the FreeBSD sandbox helper.
    #[usage(trailing_var_arg)]
    pub command: Vec<String>,
}

/// Entry point for the FreeBSD sandbox helper.
///
/// 1. Resolve the executable before any restrictive sandbox setup.
/// 2. Apply procctl hardening and supported ABI-1 network/process restrictions.
/// 3. `fexecve` into the final command.
pub fn run_main() -> ! {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--warden-launch") {
        crate::warden_launch::run_cli(args);
    }
    // Capture before parsing or opening other descriptors. The single-threaded
    // helper must not let an ambient launcher capability reach a target exec.
    let warden = unsafe { crate::warden_client::take_helper_channel() }.unwrap_or_else(|error| {
        eprintln!("alcatraz: Warden bootstrap: {error}");
        std::process::exit(1);
    });
    let CapsicumCommand {
        sandbox_policy_cwd,
        sandbox_policy,
        vfs_policy,
        socket_policy,
        allow_network_for_proxy,
        command,
    } = CapsicumCommand::parse();

    if command.is_empty() {
        eprintln!("alcatraz: no command specified to execute.");
        std::process::exit(1);
    }

    let EffectiveSandboxPolicies {
        sandbox_policy: _sandbox_policy,
        vfs_policy,
        socket_policy,
    } = resolve_sandbox_policies(
        sandbox_policy_cwd.as_path(),
        sandbox_policy,
        vfs_policy,
        socket_policy,
    )
    .unwrap_or_else(|err| {
        eprintln!("alcatraz: {err}");
        std::process::exit(1);
    });

    if let Some(channel) = warden {
        if !allow_network_for_proxy
            && (!chaos_parole::sandbox::has_full_disk_read_access(&vfs_policy)
                || !chaos_parole::sandbox::has_full_disk_write_access(&vfs_policy))
        {
            crate::warden_helper::run(
                channel,
                vfs_policy,
                socket_policy,
                sandbox_policy_cwd,
                command,
            );
        }
        // Unrestricted/proxy paths never retain the root control capability.
        drop(channel);
    }

    let prepared_exec = prepare_exec(command);
    let prepared_exec = prepared_exec.close_inherited_descriptors();

    // apply_sandbox_policy_to_current_thread applies procctl hardening and
    // then rejects unsupported restrictions to avoid fail-open behavior.
    if let Err(e) = apply_sandbox_policy_to_current_thread(
        &vfs_policy,
        socket_policy,
        allow_network_for_proxy,
        /*proxy_routed_network*/ allow_network_for_proxy,
        &sandbox_policy_cwd,
    ) {
        eprintln!("alcatraz: {e}");
        std::process::exit(1);
    }

    prepared_exec.exec_or_panic();
}

struct PreparedExec {
    executable_fd: OwnedFd,
    executable_label: String,
    command: Vec<String>,
}

impl PreparedExec {
    /// Descriptor capabilities inherited from an unrestricted parent must not
    /// bypass the new path rules. Keep only stdio and the exec-only fd at 3.
    fn close_inherited_descriptors(mut self) -> Self {
        let fd = self.executable_fd.as_raw_fd();
        if fd != 3 {
            // SAFETY: owned executable descriptor is valid, 3 is not stdio.
            if unsafe { libc::dup2(fd, 3) } < 0 {
                panic!("dup2 executable: {}", std::io::Error::last_os_error());
            }
            // SAFETY: dup2 gave us ownership of descriptor 3.
            self.executable_fd = unsafe { OwnedFd::from_raw_fd(3) };
        }
        // dup2 clears CLOEXEC. Restore it so the target cannot inherit fd 3.
        if unsafe { libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            panic!("fcntl executable: {}", std::io::Error::last_os_error());
        }
        unsafe extern "C" {
            fn closefrom(lowfd: libc::c_int);
        }
        // SAFETY: single-threaded helper, all remaining descriptors disposable.
        unsafe { closefrom(4) };
        self
    }

    fn exec_or_panic(self) -> ! {
        let c_args: Vec<CString> = self
            .command
            .iter()
            .map(|arg| c_string_from_str(arg.as_str(), "command arg"))
            .collect();
        let c_env: Vec<CString> = collect_envp();

        let mut c_args_ptrs: Vec<*const libc::c_char> =
            c_args.iter().map(|arg| arg.as_ptr()).collect();
        c_args_ptrs.push(std::ptr::null());

        let mut c_env_ptrs: Vec<*const libc::c_char> =
            c_env.iter().map(|entry| entry.as_ptr()).collect();
        c_env_ptrs.push(std::ptr::null());

        unsafe {
            libc::fexecve(
                self.executable_fd.as_raw_fd(),
                c_args_ptrs.as_ptr(),
                c_env_ptrs.as_ptr(),
            );
        }

        let err = std::io::Error::last_os_error();
        panic!("Failed to fexecve {}: {err}", self.executable_label);
    }
}

fn prepare_exec(command: Vec<String>) -> PreparedExec {
    let executable_label = command[0].clone();
    let executable_fd = resolve_executable_fd(executable_label.as_str())
        .unwrap_or_else(|err| panic!("Failed to resolve executable {executable_label}: {err}"));

    PreparedExec {
        executable_fd,
        executable_label,
        command,
    }
}

fn resolve_executable_fd(program: &str) -> std::io::Result<OwnedFd> {
    let path = std::env::var_os("PATH").or_else(default_exec_path);
    resolve_executable_fd_with_search_path(program, path.as_deref())
}

fn resolve_executable_fd_with_search_path(
    program: &str,
    search_path: Option<&OsStr>,
) -> std::io::Result<OwnedFd> {
    resolve_candidate(program, search_path).map(|(fd, _)| fd)
}

pub(crate) fn resolve_executable_path(program: &str) -> std::io::Result<PathBuf> {
    let path = std::env::var_os("PATH").or_else(default_exec_path);
    let (_, candidate) = resolve_candidate(program, path.as_deref())?;
    std::fs::canonicalize(candidate)
}

fn resolve_candidate(
    program: &str,
    search_path: Option<&OsStr>,
) -> std::io::Result<(OwnedFd, PathBuf)> {
    if program.contains('/') {
        return open_executable_fd(Path::new(program)).map(|fd| (fd, PathBuf::from(program)));
    }

    let search_path = search_path.unwrap_or_else(|| OsStr::new("/bin:/usr/bin"));
    let mut permission_error = None;

    for dir in std::env::split_paths(search_path) {
        let candidate = if dir.as_os_str().is_empty() {
            PathBuf::from(program)
        } else {
            dir.join(program)
        };

        match open_executable_fd(candidate.as_path()) {
            Ok(fd) => return Ok((fd, candidate)),
            Err(err) => match err.raw_os_error() {
                Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EISDIR) => {
                    if permission_error.is_none() {
                        permission_error = Some(err);
                    }
                }
                Some(libc::ENOENT) | Some(libc::ENOTDIR) => {}
                _ => return Err(err),
            },
        }
    }

    Err(permission_error.unwrap_or_else(|| std::io::Error::from_raw_os_error(libc::ENOENT)))
}

fn default_exec_path() -> Option<std::ffi::OsString> {
    let len = unsafe { libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0) };
    if len == 0 {
        return None;
    }

    let mut buffer = vec![0_u8; len];
    let written =
        unsafe { libc::confstr(libc::_CS_PATH, buffer.as_mut_ptr().cast(), buffer.len()) };
    if written == 0 {
        return None;
    }

    if buffer.last() == Some(&0) {
        let _ = buffer.pop();
    }

    Some(std::ffi::OsString::from_vec(buffer))
}

fn open_executable_fd(path: &Path) -> std::io::Result<OwnedFd> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path contains interior NUL bytes: {}", path.display()),
        )
    })?;

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_EXEC | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn collect_envp() -> Vec<CString> {
    std::env::vars_os()
        .map(|(key, value)| {
            let mut entry = key.into_vec();
            entry.push(b'=');
            entry.extend(value.into_vec());
            c_string_from_bytes(entry, "environment entry")
        })
        .collect()
}

fn c_string_from_str(value: &str, field: &str) -> CString {
    match CString::new(value) {
        Ok(value) => value,
        Err(_) => panic!("{field} contains interior NUL bytes"),
    }
}

fn c_string_from_bytes(value: Vec<u8>, field: &str) -> CString {
    match CString::new(value) {
        Ok(value) => value,
        Err(_) => panic!("{field} contains interior NUL bytes"),
    }
}

#[cfg(test)]
mod tests;
