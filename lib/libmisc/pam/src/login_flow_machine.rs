//! One owner drives login state and its scoped work. The update stream is a
//! projection of committed entry hooks, not a second procedural lifecycle.
use crate::{
    DeviceCode, ServerOptions, ShutdownHandle, complete_device_code_login, request_device_code,
    run_login_server,
};
use chaos_ipc::api::AuthMode;
use state_machines::{
    runtime::{Clock, Runner},
    state_machine,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct LoginContext {
    opts: ServerOptions,
    updates: mpsc::UnboundedSender<LoginFlowUpdate>,
    allow_browser_fallback: bool,
}

state_machine! {
    name: LoginFlowLifecycle,
    dynamic: true,
    context: LoginContext,
    initial: Idle,
    states: [
        superstate Live {
            state Idle,
            state WaitingForBrowser,
            state RequestingDeviceCode,
            state WaitingForDeviceCode(DeviceCode),
        },
        Succeeded, Failed(String), Cancelled,
    ],
    final_states: [Succeeded, Failed, Cancelled],
    lifecycle: {
        RequestingDeviceCode { enter: [device_pending] }
        WaitingForDeviceCode { enter: [device_ready] }
        Succeeded { enter: [succeeded] }
        Failed { enter: [failed] }
        Cancelled { enter: [cancelled] }
    },
    runtime: {
        WaitingForBrowser { invoke: [browser_work] }
        RequestingDeviceCode { invoke: [request_work] }
        WaitingForDeviceCode { invoke: [device_work] }
    },
    events {
        start_browser { transition: { from: Idle, to: WaitingForBrowser } }
        start_device_code { transition: { from: Idle, to: RequestingDeviceCode } }
        device_code_ready {
            payload: Option<DeviceCode>,
            transition: { from: RequestingDeviceCode, to: WaitingForDeviceCode, data: own_code }
        }
        device_code_unsupported {
            transition: { from: RequestingDeviceCode, to: WaitingForBrowser, before: [unsupported] }
        }
        succeed {
            transition: { from: WaitingForBrowser, to: Succeeded }
            transition: { from: WaitingForDeviceCode, to: Succeeded }
        }
        fail {
            payload: Option<String>,
            transition: { from: Live, to: Failed, data: own_error }
        }
        cancel { transition: { from: Live, to: Cancelled } }
    }
}

/// Dropping a scoped future must also stop the detached callback server.
/// Dropping a Tokio JoinHandle alone would leave that server running.
struct BrowserShutdown(ShutdownHandle);
impl Drop for BrowserShutdown {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

impl<S> LoginFlowLifecycle<S> {
    fn emit(&self, update: LoginFlowUpdate) {
        let _ = self.ctx.updates.send(update);
    }
    fn device_pending(&self) {
        self.emit(LoginFlowUpdate::DeviceCodePending);
    }
    fn device_ready(&self) {
        let device_code = self
            .state_data_waiting_for_device_code()
            .unwrap_or_else(|| unreachable!("device entry owns its code"))
            .clone();
        self.emit(LoginFlowUpdate::DeviceCodeReady { device_code });
    }
    fn unsupported(&self) {
        self.emit(LoginFlowUpdate::DeviceCodeUnsupported);
    }
    fn succeeded(&self) {
        self.emit(LoginFlowUpdate::Succeeded {
            auth_mode: AuthMode::Chatgpt,
        });
    }
    fn failed(&self) {
        let message = self
            .state_data_failed()
            .unwrap_or_else(|| unreachable!("failed entry owns its error"))
            .clone();
        self.emit(LoginFlowUpdate::Failed { message });
    }
    fn cancelled(&self) {
        self.emit(LoginFlowUpdate::Cancelled);
    }
    fn own_code(&self, code: &mut Option<DeviceCode>) -> DeviceCode {
        code.take()
            .unwrap_or_else(|| unreachable!("device completion carries its code"))
    }
    fn own_error(&self, message: &mut Option<String>) -> String {
        message
            .take()
            .unwrap_or_else(|| unreachable!("failure carries its message"))
    }
    fn browser_work(
        &self,
    ) -> impl std::future::Future<Output = LoginFlowLifecycleEvent> + Send + 'static {
        let opts = self.ctx.opts.clone();
        let updates = self.ctx.updates.clone();
        async move {
            let server = match run_login_server(opts) {
                Ok(server) => server,
                Err(error) => return LoginFlowLifecycleEvent::Fail(Some(error.to_string())),
            };
            let _shutdown = BrowserShutdown(server.cancel_handle());
            let _ = updates.send(LoginFlowUpdate::BrowserOpened {
                actual_port: server.actual_port,
                auth_url: server.auth_url.clone(),
            });
            match server.block_until_done().await {
                Ok(()) => LoginFlowLifecycleEvent::Succeed,
                Err(error) => LoginFlowLifecycleEvent::Fail(Some(error.to_string())),
            }
        }
    }
    fn request_work(
        &self,
    ) -> impl std::future::Future<Output = LoginFlowLifecycleEvent> + Send + 'static {
        let opts = self.ctx.opts.clone();
        let allow_browser_fallback = self.ctx.allow_browser_fallback;
        async move {
            match request_device_code(&opts).await {
                Ok(code) => LoginFlowLifecycleEvent::DeviceCodeReady(Some(code)),
                Err(error)
                    if allow_browser_fallback && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    LoginFlowLifecycleEvent::DeviceCodeUnsupported
                }
                Err(error) => LoginFlowLifecycleEvent::Fail(Some(error.to_string())),
            }
        }
    }
    fn device_work(
        &self,
    ) -> impl std::future::Future<Output = LoginFlowLifecycleEvent> + Send + 'static {
        let opts = self.ctx.opts.clone();
        let code = self
            .state_data_waiting_for_device_code()
            .unwrap_or_else(|| unreachable!("device activity owns its code"))
            .clone();
        async move {
            match complete_device_code_login(opts, code).await {
                Ok(()) => LoginFlowLifecycleEvent::Succeed,
                Err(error) => LoginFlowLifecycleEvent::Fail(Some(error.to_string())),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFlowMode {
    Browser,
    DeviceCode { allow_browser_fallback: bool },
}

/// Cancellation is remembered even when requested before the driver is polled.
#[derive(Debug, Clone)]
pub struct LoginFlowCancel {
    token: CancellationToken,
}
impl LoginFlowCancel {
    pub fn cancel(&self) {
        self.token.cancel();
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

// No deadlines are declared by this flow. Entry setup still needs an explicit clock.
struct LoginClock;
impl Clock for LoginClock {
    fn now(&self) -> u64 {
        0
    }
}

pub fn spawn_login_flow(opts: ServerOptions, mode: LoginFlowMode) -> LoginFlowHandle {
    start_login_flow(opts, mode).0
}

fn start_login_flow(
    opts: ServerOptions,
    mode: LoginFlowMode,
) -> (LoginFlowHandle, tokio::task::JoinHandle<()>) {
    let cancel = LoginFlowCancel {
        token: CancellationToken::new(),
    };
    let (updates, rx) = mpsc::unbounded_channel();
    let token = cancel.token.clone();
    let driver = tokio::spawn(async move {
        let context = LoginContext {
            opts,
            updates,
            allow_browser_fallback: matches!(
                mode,
                LoginFlowMode::DeviceCode {
                    allow_browser_fallback: true
                }
            ),
        };
        let mut machine = DynamicLoginFlowLifecycle::new(context);
        let start = match mode {
            LoginFlowMode::Browser => LoginFlowLifecycleEvent::StartBrowser,
            LoginFlowMode::DeviceCode { .. } => LoginFlowLifecycleEvent::StartDeviceCode,
        };
        // Commit start without leaving a queued event behind cancellation.
        if !token.is_cancelled() {
            machine
                .handle(start)
                .unwrap_or_else(|error| panic!("login start: {error:?}"));
        }
        let mut runner = Runner::new(machine, 8);
        runner
            .start(&LoginClock)
            .unwrap_or_else(|error| panic!("login entry setup: {error:?}"));
        loop {
            // Cancellation discards unconsumed completions before committing
            // the terminal edge. Extraction closes sinks and drops owned work.
            if token.is_cancelled() {
                let mut machine = runner.into_machine();
                machine
                    .handle(LoginFlowLifecycleEvent::Cancel)
                    .unwrap_or_else(|error| panic!("login cancellation: {error:?}"));
                break;
            }
            runner
                .drain(16)
                .await
                .unwrap_or_else(|error| panic!("login dispatch: {error:?}"));
            if runner.machine().is_finished() {
                break;
            }
            tokio::select! {
                biased;
                _ = token.cancelled() => {}
                result = runner.wait_for_work() => {
                    result.unwrap_or_else(|error| panic!("login wait: {error:?}"));
                }
            }
        }
    });
    (
        LoginFlowHandle {
            cancel,
            updates: rx,
        },
        driver,
    )
}

#[cfg(test)]
mod tests;
