use std::os::unix::process::CommandExt;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::LazyLock;

use chaos_ipc::product::OS_NAME;
use state_machines::state_machine;
use tracing::warn;

static ASSERTION_REASON: LazyLock<String> =
    LazyLock::new(|| format!("{OS_NAME} is running an active turn"));
const APP_ID: &str = "chaos";
// Keep the blocker process alive "long enough" without needing restarts.
// This is `i32::MAX` seconds, which is accepted by common `sleep` implementations.
const BLOCKER_SLEEP_SECONDS: &str = "2147483647";

pub(crate) struct LinuxSleepInhibitor {
    machine: DynamicInhibitorLifecycle<()>,
    preferred_backend: Option<LinuxBackend>,
    missing_backend_logged: bool,
}

pub(crate) use LinuxSleepInhibitor as SleepInhibitor;

#[derive(Debug)]
pub(crate) struct BackendProcess {
    backend: LinuxBackend,
    child: Child,
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        if let Err(error) = self.child.kill()
            && !child_exited(&error)
        {
            warn!(backend = ?self.backend, reason = %error, "Failed to stop Linux sleep inhibitor backend");
        }
        if let Err(error) = self.child.wait()
            && !child_exited(&error)
        {
            warn!(backend = ?self.backend, reason = %error, "Failed to reap Linux sleep inhibitor backend");
        }
    }
}

state_machine! {
    name: InhibitorLifecycle,
    dynamic: true,
    initial: Inactive,
    states: [Inactive, Active(BackendProcess)],
    events {
        acquire {
            payload: Option<BackendProcess>,
            transition: { from: Inactive, to: Active, data: own_process }
        }
        release {
            transition: { from: Active, to: Inactive }
            transition: { from: Inactive, internal: true }
        }
    }
}

impl<C, S> InhibitorLifecycle<C, S> {
    fn own_process(&self, process: &mut Option<BackendProcess>) -> BackendProcess {
        process
            .take()
            .unwrap_or_else(|| unreachable!("acquire owns inhibitor process"))
    }
}

impl Default for LinuxSleepInhibitor {
    fn default() -> Self {
        Self {
            machine: InhibitorLifecycle::new(()).into_dynamic(),
            preferred_backend: None,
            missing_backend_logged: false,
        }
    }
}

impl std::fmt::Debug for LinuxSleepInhibitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxSleepInhibitor")
            .field("state", &self.machine.current_state())
            .field("preferred_backend", &self.preferred_backend)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
enum LinuxBackend {
    SystemdInhibit,
    GnomeSessionInhibit,
}

impl LinuxSleepInhibitor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn acquire(&mut self) {
        if let Some(process) = self.machine.active_data_mut() {
            let backend = process.backend;
            match process.child.try_wait() {
                Ok(None) => return,
                Ok(Some(status)) => {
                    warn!(
                        ?backend,
                        ?status,
                        "Linux sleep inhibitor backend exited unexpectedly; attempting fallback"
                    );
                }
                Err(error) => {
                    warn!(
                        ?backend,
                        reason = %error,
                        "Failed to query Linux sleep inhibitor backend status; attempting restart"
                    );
                }
            }
        }

        self.release();
        let should_log_backend_failures = !self.missing_backend_logged;
        let backends = match self.preferred_backend {
            Some(LinuxBackend::SystemdInhibit) => [
                LinuxBackend::SystemdInhibit,
                LinuxBackend::GnomeSessionInhibit,
            ],
            Some(LinuxBackend::GnomeSessionInhibit) => [
                LinuxBackend::GnomeSessionInhibit,
                LinuxBackend::SystemdInhibit,
            ],
            None => [
                LinuxBackend::SystemdInhibit,
                LinuxBackend::GnomeSessionInhibit,
            ],
        };

        for backend in backends {
            match spawn_backend(backend) {
                Ok(child) => {
                    let mut process = BackendProcess { backend, child };
                    match process.child.try_wait() {
                        Ok(None) => {
                            assert!(
                                self.machine
                                    .handle(InhibitorLifecycleEvent::Acquire(Some(process)))
                                    .is_ok()
                            );
                            self.preferred_backend = Some(backend);
                            self.missing_backend_logged = false;
                            return;
                        }
                        Ok(Some(status)) => {
                            if should_log_backend_failures {
                                warn!(
                                    ?backend,
                                    ?status,
                                    "Linux sleep inhibitor backend exited immediately"
                                );
                            }
                        }
                        Err(error) => {
                            if should_log_backend_failures {
                                warn!(
                                    ?backend,
                                    reason = %error,
                                    "Failed to query Linux sleep inhibitor backend status after spawn"
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    if should_log_backend_failures && error.kind() != std::io::ErrorKind::NotFound {
                        warn!(
                            ?backend,
                            reason = %error,
                            "Failed to start Linux sleep inhibitor backend"
                        );
                    }
                }
            }
        }

        if should_log_backend_failures {
            warn!("No Linux sleep inhibitor backend is available");
            self.missing_backend_logged = true;
        }
    }

    pub(crate) fn release(&mut self) {
        assert!(
            self.machine
                .handle(InhibitorLifecycleEvent::Release)
                .is_ok()
        );
    }
}

fn spawn_backend(backend: LinuxBackend) -> Result<Child, std::io::Error> {
    // Ensure the helper receives SIGTERM when the original parent dies.
    // `parent_pid` is captured before spawn and checked in `pre_exec` to avoid
    // the fork/exec race where the parent exits before PDEATHSIG is armed.
    // SAFETY: `getpid` has no preconditions and is safe to call here.
    let parent_pid = unsafe { libc::getpid() };
    let mut command = match backend {
        LinuxBackend::SystemdInhibit => {
            let mut command = Command::new("systemd-inhibit");
            command.args([
                "--what=idle",
                "--mode=block",
                "--who",
                APP_ID,
                "--why",
                ASSERTION_REASON.as_str(),
                "--",
                "sleep",
                BLOCKER_SLEEP_SECONDS,
            ]);
            command
        }
        LinuxBackend::GnomeSessionInhibit => {
            let mut command = Command::new("gnome-session-inhibit");
            command.args([
                "--inhibit",
                "idle",
                "--reason",
                ASSERTION_REASON.as_str(),
                "sleep",
                BLOCKER_SLEEP_SECONDS,
            ]);
            command
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: `pre_exec` must be registered before spawn. The closure only
    // performs libc setup for the child process and returns an `io::Error`
    // when parent-death signal setup fails.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                libc::raise(libc::SIGTERM);
            }
            Ok(())
        });
    }

    command.spawn()
}

fn child_exited(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::InvalidInput)
}

#[cfg(test)]
mod tests;
