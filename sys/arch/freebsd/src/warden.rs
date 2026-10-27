//! Warden: one-command ownership for a dedicated privileged worker. This is not a
//! public root daemon or an authenticated transport: the launcher must supply
//! the command identity, never trust one embedded in an untrusted request.
//!
//! Only the initial all-read-only filesystem subset is executable. The direct
//! unprivileged helper remains fail-closed without a valid root bootstrap.

use crate::gulag::PreparedPolicy;
use crate::warden_jail::OwnedJail;
use crate::warden_mount::ScopedMounts;
use crate::warden_plan::{self, FilesystemPlan};
use crate::warden_process::{self, CommandIdentity};
use chaos_ipc::protocol::{SocketPolicy, VfsPolicy};
use std::collections::BTreeMap;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    pub filesystem: VfsPolicy,
    pub network: SocketPolicy,
    pub argv: Vec<String>,
    /// Complete target environment; Warden's environment is cleared.
    pub environment: BTreeMap<String, String>,
    pub cwd: PathBuf,
    /// Base used to interpret the filesystem policy, not necessarily exec cwd.
    pub policy_cwd: PathBuf,
}

impl CommandRequest {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid or oversized jailed command",
            )
        };
        if self.argv.is_empty()
            || self.argv.len() > 4096
            || self.environment.len() > 4096
            || !Path::new(&self.argv[0]).is_absolute()
            || !self.cwd.is_absolute()
            || !self.policy_cwd.is_absolute()
            || self.cwd.as_os_str().as_bytes().contains(&0)
            || self.policy_cwd.as_os_str().as_bytes().contains(&0)
            || self
                .cwd
                .components()
                .any(|part| part == Component::ParentDir)
        {
            return Err(invalid());
        }
        let mut total = 0usize;
        for value in self
            .argv
            .iter()
            .chain(self.environment.values())
            .chain(self.environment.keys())
        {
            if value.as_bytes().contains(&0) {
                return Err(invalid());
            }
            total = total.checked_add(value.len() + 1).ok_or_else(invalid)?;
            if total > 1024 * 1024 {
                return Err(invalid());
            }
        }
        if self
            .environment
            .keys()
            .any(|name| name.is_empty() || name.contains('='))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Pipes are the only deliberate capabilities given to the command. The
/// caller must concurrently relay/drain output while polling exit/cancel.
pub struct CommandIo {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

#[must_use = "poll exit or cancel, then explicitly report cleanup completion"]
pub struct CommandSession {
    child: Child,
    jail: Option<OwnedJail>,
    mounts: ScopedMounts,
    status: Option<ExitStatus>,
}

impl CommandSession {
    /// Dedicated single-threaded root worker only. Identity is authenticated
    /// launcher state, not a request field. No automatic module load or sudo.
    pub fn start(
        request: &CommandRequest,
        identity: &CommandIdentity,
        state_parent: &Path,
    ) -> io::Result<Self> {
        request.validate()?;
        Self::start_with_stdio(request, identity, state_parent, None)
    }

    pub(crate) fn start_with_stdio(
        request: &CommandRequest,
        identity: &CommandIdentity,
        state_parent: &Path,
        stdio: Option<[OwnedFd; 3]>,
    ) -> io::Result<Self> {
        request.validate()?;
        let plan = warden_plan::compile(&request.filesystem, request.network, &request.policy_cwd)
            .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error.to_string()))?;
        if !matches!(&plan.filesystem, FilesystemPlan::ReadOnlyHostView { overlays } if overlays.is_empty())
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "only all-read-only supervised commands are implemented",
            ));
        }
        // Fail before creating any mounts when the required driver is absent.
        let policy = PreparedPolicy::new(request.network)
            .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error.to_string()))?;
        Self::start_inner_with_stdio(
            request,
            identity,
            state_parent,
            &plan.filesystem,
            Some(policy),
            stdio,
        )
    }

    #[cfg(test)]
    fn start_inner(
        request: &CommandRequest,
        identity: &CommandIdentity,
        state_parent: &Path,
        filesystem: &FilesystemPlan,
        policy: Option<PreparedPolicy>,
    ) -> io::Result<Self> {
        Self::start_inner_with_stdio(request, identity, state_parent, filesystem, policy, None)
    }

    fn start_inner_with_stdio(
        request: &CommandRequest,
        identity: &CommandIdentity,
        state_parent: &Path,
        filesystem: &FilesystemPlan,
        policy: Option<PreparedPolicy>,
        stdio: Option<[OwnedFd; 3]>,
    ) -> io::Result<Self> {
        let (mounts, root) = crate::warden_view::provision(filesystem, state_parent)?;
        let name = mounts
            .state_path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("invalid private state name"))?;
        let jail = OwnedJail::create(root, name, request.network)?;
        let mut command = Command::new(&request.argv[0]);
        command
            .args(&request.argv[1..])
            .env_clear()
            .envs(&request.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some([stdin, stdout, stderr]) = stdio {
            command
                .stdin(Stdio::from(stdin))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
        }
        let child = warden_process::spawn_inner(&jail, command, identity, &request.cwd, policy)?;
        Ok(Self {
            child,
            jail: Some(jail),
            mounts,
            status: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn start_transport_test(
        request: &CommandRequest,
        identity: &CommandIdentity,
        parent: &Path,
        stdio: [OwnedFd; 3],
    ) -> io::Result<Self> {
        request.validate()?;
        let plan = warden_plan::compile(&request.filesystem, request.network, &request.policy_cwd)
            .map_err(|error| io::Error::other(error.to_string()))?;
        Self::start_inner_with_stdio(
            request,
            identity,
            parent,
            &plan.filesystem,
            None,
            Some(stdio),
        )
    }

    pub fn state_path(&self) -> &Path {
        self.mounts.state_path()
    }

    pub fn take_io(&mut self) -> CommandIo {
        CommandIo {
            stdin: self.child.stdin.take(),
            stdout: self.child.stdout.take(),
            stderr: self.child.stderr.take(),
        }
    }

    /// Observing primary exit immediately removes its jail, so background
    /// descendants cannot keep output pipes or the filesystem view alive.
    pub fn poll_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        if self.status.is_some() {
            self.begin_remove();
        }
        Ok(self.status)
    }

    /// Call after primary exit. Returns WouldBlock rather than cancelling a
    /// still-running primary command. Success means all owned mounts are gone.
    pub fn finish_cleanup(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        if self.poll_exit()?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "command has not exited",
            ));
        }
        self.reap_and_cleanup(timeout)
    }

    /// Object-bound process-tree cancellation; never targets a recycled PID
    /// or numeric JID. Bounded wait, no force-unmount, journal retained on error.
    pub fn cancel_and_cleanup(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        self.begin_remove();
        self.reap_and_cleanup(timeout)
    }

    fn begin_remove(&mut self) {
        if let Some(jail) = self.jail.take() {
            drop(jail.begin_remove());
        }
    }

    fn reap_and_cleanup(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "cleanup timeout overflow")
        })?;
        loop {
            if let Some(status) = self.poll_exit()? {
                match self.mounts.cleanup() {
                    Ok(()) => return Ok(status),
                    Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {}
                    Err(error) => return Err(error),
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "command teardown incomplete; retain the private mount journal for recovery",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for CommandSession {
    fn drop(&mut self) {
        self.begin_remove();
        let _ = self.child.try_wait();
        // ScopedMounts tries non-forced cleanup, retaining/reporting its journal
        // if asynchronous process removal still pins the view. Dedicated worker
        // exit lets init reap a child not yet waitable here.
    }
}

#[cfg(test)]
mod tests;
