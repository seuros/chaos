//! Minimal unprivileged harness exercising early capture + PreparedCommand.
//! Requires a private root-created bootstrap, exactly like the real harness.
//! No policy/driver bypass exists here; native tests supply their test worker.
//! Optional `--check PROBE MARKER` runs the combined live confinement probe.

#[cfg(target_os = "freebsd")]
mod native {
    use alcatraz_base::prepared_command::SandboxRequest;
    use alcatraz_freebsd::warden_client;
    use chaos_ipc::protocol::{SocketPolicy, VfsPolicy};
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};

    pub fn run() -> io::Result<()> {
        // This executable has no runtime or worker threads yet.
        unsafe {
            warden_client::initialize()?;
        }
        if std::env::var_os(warden_client::CONTROL_ENV).is_some() {
            return Err(io::Error::other(
                "ambient Warden capability was not removed",
            ));
        }
        let mut args = std::env::args_os().skip(1);
        let helper = args
            .next()
            .ok_or_else(|| io::Error::other("expected absolute Alcatraz helper path"))?;
        let (target, expected) = match args.next() {
            None => (
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'harness-probe\\n'; exit 37".into(),
                ],
                37,
            ),
            Some(mode) if mode == "--check" => {
                let probe = args
                    .next()
                    .ok_or_else(|| io::Error::other("missing confinement probe"))?;
                let marker = args
                    .next()
                    .ok_or_else(|| io::Error::other("missing write-refusal marker"))?;
                if args.next().is_some() {
                    return Err(io::Error::other("unexpected probe arguments"));
                }
                (
                    vec![
                        probe
                            .into_string()
                            .map_err(|_| io::Error::other("non-UTF8 probe"))?,
                        "warden-check".into(),
                        marker
                            .into_string()
                            .map_err(|_| io::Error::other("non-UTF8 marker"))?,
                    ],
                    0,
                )
            }
            Some(_) => return Err(io::Error::other("unknown probe mode")),
        };
        let vfs = VfsPolicy::default();
        let prepared = alcatraz_freebsd::prepare_command(SandboxRequest {
            executable: Path::new(&helper),
            command: target,
            file_system_policy: &vfs,
            network_policy: SocketPolicy::Restricted,
            sandbox_policy_cwd: Path::new("/"),
            enforce_managed_network: false,
            network: None,
            platform_permissions: None,
        })?;
        let fd = warden_client::inherited_fd(
            prepared.arg0.as_deref(),
            prepared
                .env
                .get(warden_client::CONTROL_ENV)
                .map(String::as_str),
        )?
        .ok_or_else(|| io::Error::other("missing captured Warden capability"))?;
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC == 0 {
            return Err(io::Error::other("parent control channel is not CLOEXEC"));
        }
        if warden_client::inherited_fd(Some("/bin/sh"), Some(&fd.to_string()))?.is_some() {
            return Err(io::Error::other(
                "control capability offered to an ordinary target",
            ));
        }
        let mut command = Command::new(&prepared.program);
        warden_client::inherit_for_helper(
            &mut command,
            prepared.arg0.as_deref(),
            prepared
                .env
                .get(warden_client::CONTROL_ENV)
                .map(String::as_str),
        )?;
        command
            .arg0("alcatraz")
            .args(&prepared.args)
            .env_clear()
            .envs(prepared.env)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let status = command.status()?;
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
            return Err(io::Error::other(
                "spawn changed the parent control descriptor",
            ));
        }
        if status.code() != Some(expected) {
            return Err(io::Error::other(format!(
                "unexpected helper status: {status}"
            )));
        }
        Ok(())
    }
}

#[cfg(target_os = "freebsd")]
fn main() -> std::io::Result<()> {
    native::run()
}

#[cfg(not(target_os = "freebsd"))]
fn main() {
    eprintln!("Warden requires FreeBSD");
}
