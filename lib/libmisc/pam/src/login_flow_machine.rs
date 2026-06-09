use crate::DeviceCode;
use crate::ServerOptions;
use crate::complete_device_code_login;
use crate::request_device_code;
use crate::run_login_server;
use chaos_ipc::api::AuthMode;
use state_machines::state_machine;
use std::io;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::mpsc;

state_machine! {
    name: LoginFlowLifecycle,
    dynamic: true,
    initial: Idle,
    states: [
        Idle,
        StartingBrowser,
        WaitingForBrowser,
        RequestingDeviceCode,
        WaitingForDeviceCode,
        Succeeded,
        Failed,
        Cancelled
    ],
    events {
        start_browser {
            transition: { from: Idle, to: StartingBrowser }
            transition: { from: Failed, to: StartingBrowser }
            transition: { from: RequestingDeviceCode, to: StartingBrowser }
        }
        browser_ready {
            transition: { from: StartingBrowser, to: WaitingForBrowser }
        }
        start_device_code {
            transition: { from: Idle, to: RequestingDeviceCode }
            transition: { from: Failed, to: RequestingDeviceCode }
        }
        device_code_ready {
            transition: { from: RequestingDeviceCode, to: WaitingForDeviceCode }
        }
        device_code_unsupported {
            transition: { from: RequestingDeviceCode, to: StartingBrowser }
        }
        succeed {
            transition: { from: WaitingForBrowser, to: Succeeded }
            transition: { from: WaitingForDeviceCode, to: Succeeded }
        }
        fail {
            transition: { from: StartingBrowser, to: Failed }
            transition: { from: WaitingForBrowser, to: Failed }
            transition: { from: RequestingDeviceCode, to: Failed }
            transition: { from: WaitingForDeviceCode, to: Failed }
        }
        cancel {
            transition: { from: StartingBrowser, to: Cancelled }
            transition: { from: WaitingForBrowser, to: Cancelled }
            transition: { from: RequestingDeviceCode, to: Cancelled }
            transition: { from: WaitingForDeviceCode, to: Cancelled }
        }
    }
}

#[derive(Debug)]
struct LoginFlowWorkflow {
    machine: DynamicLoginFlowLifecycle<()>,
}

impl LoginFlowWorkflow {
    fn new() -> Self {
        Self {
            machine: DynamicLoginFlowLifecycle::new(()),
        }
    }

    /// Apply a transition that the runner sequences and that must always be
    /// legal from the current state. A rejected transition means the runner and
    /// the emitted `LoginFlowUpdate` stream have drifted, so fail loudly in
    /// debug/test builds rather than silently no-op while the matching update
    /// still goes out.
    fn apply(&mut self, event: LoginFlowLifecycleEvent, label: &str) {
        if self.machine.handle(event).is_err() {
            debug_assert!(
                false,
                "illegal LoginFlow transition: {label} from {}",
                self.machine.current_state()
            );
        }
    }

    fn start_browser(&mut self) {
        self.apply(LoginFlowLifecycleEvent::StartBrowser, "start_browser");
    }

    fn browser_ready(&mut self) {
        self.apply(LoginFlowLifecycleEvent::BrowserReady, "browser_ready");
    }

    fn start_device_code(&mut self) {
        self.apply(
            LoginFlowLifecycleEvent::StartDeviceCode,
            "start_device_code",
        );
    }

    fn device_code_ready(&mut self) {
        self.apply(
            LoginFlowLifecycleEvent::DeviceCodeReady,
            "device_code_ready",
        );
    }

    fn device_code_unsupported(&mut self) {
        self.apply(
            LoginFlowLifecycleEvent::DeviceCodeUnsupported,
            "device_code_unsupported",
        );
    }

    fn succeed(&mut self) {
        self.apply(LoginFlowLifecycleEvent::Succeed, "succeed");
    }

    fn fail(&mut self) {
        self.apply(LoginFlowLifecycleEvent::Fail, "fail");
    }

    fn cancel(&mut self) {
        self.apply(LoginFlowLifecycleEvent::Cancel, "cancel");
    }

    #[cfg(test)]
    fn current_state(&self) -> &str {
        self.machine.current_state()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFlowMode {
    Browser,
    DeviceCode { allow_browser_fallback: bool },
}

#[derive(Debug, Clone)]
pub struct LoginFlowCancel {
    notify: Arc<Notify>,
}

impl LoginFlowCancel {
    pub fn cancel(&self) {
        self.notify.notify_waiters();
    }

    async fn notified(&self) {
        self.notify.notified().await;
    }
}

#[derive(Debug, Clone)]
pub enum LoginFlowUpdate {
    DeviceCodePending,
    DeviceCodeUnsupported,
    BrowserOpened { actual_port: u16, auth_url: String },
    DeviceCodeReady { device_code: DeviceCode },
    Succeeded { auth_mode: AuthMode },
    Failed { message: String },
    Cancelled,
}

#[derive(Debug)]
pub struct LoginFlowHandle {
    cancel: LoginFlowCancel,
    updates: mpsc::UnboundedReceiver<LoginFlowUpdate>,
}

impl LoginFlowHandle {
    pub fn cancel_handle(&self) -> LoginFlowCancel {
        self.cancel.clone()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub async fn recv(&mut self) -> Option<LoginFlowUpdate> {
        self.updates.recv().await
    }
}

pub fn spawn_login_flow(opts: ServerOptions, mode: LoginFlowMode) -> LoginFlowHandle {
    let cancel = LoginFlowCancel {
        notify: Arc::new(Notify::new()),
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let flow_cancel = cancel.clone();

    tokio::spawn(async move {
        let mut runner = LoginFlowRunner::new(opts, flow_cancel, tx);
        match mode {
            LoginFlowMode::Browser => runner.run_browser_flow().await,
            LoginFlowMode::DeviceCode {
                allow_browser_fallback,
            } => runner.run_device_code_flow(allow_browser_fallback).await,
        }
    });

    LoginFlowHandle {
        cancel,
        updates: rx,
    }
}

struct LoginFlowRunner {
    opts: ServerOptions,
    cancel: LoginFlowCancel,
    tx: mpsc::UnboundedSender<LoginFlowUpdate>,
    workflow: LoginFlowWorkflow,
}

impl LoginFlowRunner {
    fn new(
        opts: ServerOptions,
        cancel: LoginFlowCancel,
        tx: mpsc::UnboundedSender<LoginFlowUpdate>,
    ) -> Self {
        Self {
            opts,
            cancel,
            tx,
            workflow: LoginFlowWorkflow::new(),
        }
    }

    fn emit(&self, update: LoginFlowUpdate) {
        let _ = self.tx.send(update);
    }

    async fn run_browser_flow(&mut self) {
        self.workflow.start_browser();
        if let Err(err) = self.begin_browser_flow().await {
            self.workflow.fail();
            self.emit(LoginFlowUpdate::Failed {
                message: err.to_string(),
            });
        }
    }

    async fn begin_browser_flow(&mut self) -> io::Result<()> {
        let server = run_login_server(self.opts.clone())?;
        let auth_url = server.auth_url.clone();
        let actual_port = server.actual_port;
        self.workflow.browser_ready();
        self.emit(LoginFlowUpdate::BrowserOpened {
            actual_port,
            auth_url,
        });
        let cancel = self.cancel.clone();
        let shutdown = server.cancel_handle();

        tokio::select! {
            _ = cancel.notified() => {
                shutdown.shutdown();
                self.workflow.cancel();
                self.emit(LoginFlowUpdate::Cancelled);
                Ok(())
            }
            result = server.block_until_done() => {
                result?;
                self.workflow.succeed();
                self.emit(LoginFlowUpdate::Succeeded { auth_mode: AuthMode::Chatgpt });
                Ok(())
            }
        }
    }

    async fn run_device_code_flow(&mut self, allow_browser_fallback: bool) {
        self.workflow.start_device_code();
        self.emit(LoginFlowUpdate::DeviceCodePending);
        let cancel = self.cancel.clone();
        let request_result = tokio::select! {
            _ = cancel.notified() => {
                self.workflow.cancel();
                self.emit(LoginFlowUpdate::Cancelled);
                return;
            }
            result = request_device_code(&self.opts) => result,
        };

        match request_result {
            Ok(device_code) => {
                self.workflow.device_code_ready();
                self.emit(LoginFlowUpdate::DeviceCodeReady {
                    device_code: device_code.clone(),
                });
                let cancel = self.cancel.clone();
                let result = tokio::select! {
                    _ = cancel.notified() => {
                        self.workflow.cancel();
                        self.emit(LoginFlowUpdate::Cancelled);
                        return;
                    }
                    result = complete_device_code_login(self.opts.clone(), device_code) => result,
                };
                match result {
                    Ok(()) => {
                        self.workflow.succeed();
                        self.emit(LoginFlowUpdate::Succeeded {
                            auth_mode: AuthMode::Chatgpt,
                        });
                    }
                    Err(err) => {
                        self.workflow.fail();
                        self.emit(LoginFlowUpdate::Failed {
                            message: err.to_string(),
                        });
                    }
                }
            }
            Err(err) if allow_browser_fallback && err.kind() == io::ErrorKind::NotFound => {
                self.workflow.device_code_unsupported();
                self.emit(LoginFlowUpdate::DeviceCodeUnsupported);
                if let Err(err) = self.begin_browser_flow().await {
                    self.workflow.fail();
                    self.emit(LoginFlowUpdate::Failed {
                        message: err.to_string(),
                    });
                }
            }
            Err(err) => {
                self.workflow.fail();
                self.emit(LoginFlowUpdate::Failed {
                    message: err.to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_flow_happy_path_states() {
        let mut wf = LoginFlowWorkflow::new();
        assert_eq!(wf.current_state(), "Idle");

        wf.start_browser();
        assert_eq!(wf.current_state(), "StartingBrowser");

        wf.browser_ready();
        assert_eq!(wf.current_state(), "WaitingForBrowser");

        wf.succeed();
        assert_eq!(wf.current_state(), "Succeeded");
    }

    #[test]
    fn device_code_can_fallback_to_browser() {
        let mut wf = LoginFlowWorkflow::new();
        wf.start_device_code();
        assert_eq!(wf.current_state(), "RequestingDeviceCode");

        wf.device_code_unsupported();
        assert_eq!(wf.current_state(), "StartingBrowser");

        wf.browser_ready();
        assert_eq!(wf.current_state(), "WaitingForBrowser");
    }

    #[test]
    fn device_code_can_be_cancelled_while_waiting() {
        let mut wf = LoginFlowWorkflow::new();
        wf.start_device_code();
        wf.device_code_ready();
        assert_eq!(wf.current_state(), "WaitingForDeviceCode");

        wf.cancel();
        assert_eq!(wf.current_state(), "Cancelled");
    }

    #[test]
    fn illegal_transition_is_rejected_and_state_unchanged() {
        // Drive the raw machine so the wrapper's debug_assert does not fire:
        // succeeding from Idle is not a declared transition.
        let mut machine = DynamicLoginFlowLifecycle::new(());
        assert_eq!(machine.current_state(), "Idle");

        assert!(machine.handle(LoginFlowLifecycleEvent::Succeed).is_err());
        assert_eq!(machine.current_state(), "Idle");
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "illegal LoginFlow transition: succeed")]
    fn wrapper_panics_when_transition_drifts() {
        // The runner must never emit a Succeeded update without a legal
        // transition; the wrapper guards that invariant in debug builds.
        let mut wf = LoginFlowWorkflow::new();
        wf.succeed();
    }
}
