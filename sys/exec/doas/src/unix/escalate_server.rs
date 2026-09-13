use std::collections::HashMap;
use std::future::Future;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context as _;
use chaos_realpath::AbsolutePathBuf;
use socket2::Socket;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::unix::escalate_protocol::ESCALATE_SOCKET_ENV_VAR;
use crate::unix::escalate_protocol::EXEC_WRAPPER_ENV_VAR;
use crate::unix::escalate_protocol::EscalateAction;
use crate::unix::escalate_protocol::EscalateRequest;
use crate::unix::escalate_protocol::EscalateResponse;
use crate::unix::escalate_protocol::EscalationDecision;
use crate::unix::escalate_protocol::EscalationExecution;
use crate::unix::escalate_protocol::SuperExecMessage;
use crate::unix::escalate_protocol::SuperExecResult;
use crate::unix::escalation_policy::EscalationPolicy;
use crate::unix::socket::AsyncDatagramSocket;
use crate::unix::socket::AsyncSocket;

/// Adapter for running the shell command after the escalation server has been set up.
///
/// This lets `shell-escalation` own the Unix escalation protocol while the caller
/// keeps control over process spawning, output capture, and sandbox integration.
/// Implementations can capture any sandbox state they need.
pub trait ShellCommandExecutor: Send + Sync {
    /// Runs the requested shell command and returns the captured result.
    ///
    /// `env_overlay` contains only the wrapper/socket variables exported by
    /// `EscalationSession::env()`, not a complete child environment.
    /// Implementations should merge it into whatever base environment they use
    /// for the shell process. `after_spawn` should be invoked immediately after
    /// the shell process has been spawned so the parent copy of the inherited
    /// escalation socket can be closed.
    fn run(
        &self,
        command: Vec<String>,
        cwd: PathBuf,
        env_overlay: HashMap<String, String>,
        cancel_rx: CancellationToken,
        after_spawn: Option<Box<dyn FnOnce() + Send>>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<ExecResult>> + Send + '_>>;

    /// Prepares an escalated subcommand for execution on the server side.
    fn prepare_escalated_exec(
        &self,
        program: &AbsolutePathBuf,
        argv: &[String],
        workdir: &AbsolutePathBuf,
        env: HashMap<String, String>,
        execution: EscalationExecution,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<PreparedExec>> + Send + '_>>;
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct ExecParams {
    /// The the string of Zsh/shell to execute.
    pub command: String,
    /// The working directory to execute the command in. Must be an absolute path.
    pub workdir: String,
    /// The timeout for the command in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Launch Bash with -lc instead of -c: defaults to true.
    pub login: Option<bool>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Aggregated stdout+stderr output for compatibility with existing callers.
    pub output: String,
    pub duration: Duration,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedExec {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub arg0: Option<String>,
}

#[derive(Debug)]
pub struct EscalationSession {
    env: HashMap<String, String>,
    task: JoinHandle<anyhow::Result<()>>,
    client_socket: Arc<Mutex<Option<Socket>>>,
    cancellation_token: CancellationToken,
}

impl EscalationSession {
    /// Returns just the environment overlay needed by the execve wrapper.
    ///
    /// Callers should merge this into their own child-process environment
    /// rather than treating it as the full environment for the shell.
    pub fn env(&self) -> &HashMap<String, String> {
        &self.env
    }

    pub fn close_client_socket(&self) {
        if let Ok(mut client_socket) = self.client_socket.lock() {
            client_socket.take();
        }
    }
}

impl Drop for EscalationSession {
    fn drop(&mut self) {
        self.close_client_socket();
        self.cancellation_token.cancel();
        self.task.abort();
    }
}

pub struct EscalateServer {
    bash_path: PathBuf,
    execve_wrapper: PathBuf,
    policy: Arc<dyn EscalationPolicy>,
}

impl EscalateServer {
    pub fn new<Policy>(bash_path: PathBuf, execve_wrapper: PathBuf, policy: Policy) -> Self
    where
        Policy: EscalationPolicy + Send + Sync + 'static,
    {
        Self {
            bash_path,
            execve_wrapper,
            policy: Arc::new(policy),
        }
    }

    pub async fn exec(
        &self,
        params: ExecParams,
        cancel_rx: CancellationToken,
        command_executor: Arc<dyn ShellCommandExecutor>,
    ) -> anyhow::Result<ExecResult> {
        let session = self.start_session(cancel_rx.clone(), Arc::clone(&command_executor))?;
        let env_overlay = session.env().clone();
        let client_socket = Arc::clone(&session.client_socket);
        let command = vec![
            self.bash_path.to_string_lossy().to_string(),
            if params.login == Some(false) {
                "-c".to_string()
            } else {
                "-lc".to_string()
            },
            params.command,
        ];
        let workdir = AbsolutePathBuf::try_from(params.workdir)?;
        let result = command_executor
            .run(
                command,
                workdir.to_path_buf(),
                env_overlay,
                cancel_rx,
                Some(Box::new(move || {
                    if let Ok(mut client_socket) = client_socket.lock() {
                        client_socket.take();
                    }
                })),
            )
            .await?;
        Ok(result)
    }

    /// Starts an escalation session and returns the environment overlay a shell
    /// needs in order to route intercepted execs through this server.
    ///
    /// This does not spawn the shell itself. Callers own process creation and
    /// only use the returned environment plus the session lifetime handle.
    pub fn start_session(
        &self,
        parent_cancellation_token: CancellationToken,
        command_executor: Arc<dyn ShellCommandExecutor>,
    ) -> anyhow::Result<EscalationSession> {
        let cancellation_token = CancellationToken::new();
        let (escalate_server, escalate_client) = AsyncDatagramSocket::pair()?;
        let client_socket = escalate_client.into_inner();
        let client_socket_fd = client_socket.as_raw_fd();
        // Only the client endpoint should cross exec into the wrapper process.
        client_socket.set_cloexec(false)?;
        let client_socket = Arc::new(Mutex::new(Some(client_socket)));
        let task = tokio::spawn(escalate_task(
            escalate_server,
            Arc::clone(&self.policy),
            Arc::clone(&command_executor),
            parent_cancellation_token,
            cancellation_token.clone(),
        ));
        let mut env = HashMap::new();
        env.insert(
            ESCALATE_SOCKET_ENV_VAR.to_string(),
            client_socket_fd.to_string(),
        );
        env.insert(
            EXEC_WRAPPER_ENV_VAR.to_string(),
            self.execve_wrapper.to_string_lossy().to_string(),
        );
        Ok(EscalationSession {
            env,
            task,
            client_socket,
            cancellation_token,
        })
    }
}

async fn escalate_task(
    socket: AsyncDatagramSocket,
    policy: Arc<dyn EscalationPolicy>,
    command_executor: Arc<dyn ShellCommandExecutor>,
    parent_cancellation_token: CancellationToken,
    session_cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    loop {
        let (_, mut fds) = tokio::select! {
            received = socket.receive_with_fds() => received?,
            _ = parent_cancellation_token.cancelled() => return Ok(()),
            _ = session_cancellation_token.cancelled() => return Ok(()),
        };
        if fds.len() != 1 {
            tracing::error!("expected 1 fd in datagram handshake, got {}", fds.len());
            continue;
        }
        let stream_socket = AsyncSocket::from_fd(fds.remove(0))?;
        let policy = Arc::clone(&policy);
        let command_executor = Arc::clone(&command_executor);
        let parent_cancellation_token = parent_cancellation_token.clone();
        let session_cancellation_token = session_cancellation_token.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_escalate_session_with_policy(
                stream_socket,
                policy,
                command_executor,
                parent_cancellation_token,
                session_cancellation_token,
            )
            .await
            {
                tracing::error!("escalate session failed: {err:?}");
            }
        });
    }
}

async fn handle_escalate_session_with_policy(
    socket: AsyncSocket,
    policy: Arc<dyn EscalationPolicy>,
    command_executor: Arc<dyn ShellCommandExecutor>,
    parent_cancellation_token: CancellationToken,
    session_cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let EscalateRequest {
        file,
        argv,
        workdir,
        env,
    } = tokio::select! {
        request = socket.receive::<EscalateRequest>() => request?,
        _ = parent_cancellation_token.cancelled() => return Ok(()),
        _ = session_cancellation_token.cancelled() => return Ok(()),
    };
    let program = AbsolutePathBuf::resolve_path_against_base(file, workdir.as_path());
    let decision = tokio::select! {
        decision = policy.determine_action(&program, &argv, &workdir) => {
            decision.context("failed to determine escalation action")?
        }
        _ = parent_cancellation_token.cancelled() => return Ok(()),
        _ = session_cancellation_token.cancelled() => return Ok(()),
    };

    tracing::debug!("decided {decision:?} for {program:?} {argv:?} {workdir:?}");

    match decision {
        EscalationDecision::Run => {
            socket
                .send(EscalateResponse {
                    action: EscalateAction::Run,
                })
                .await?;
        }
        EscalationDecision::Escalate(execution) => {
            socket
                .send(EscalateResponse {
                    action: EscalateAction::Escalate,
                })
                .await?;
            let (msg, fds) = tokio::select! {
                message = socket.receive_with_fds::<SuperExecMessage>() => {
                    message.context("failed to receive SuperExecMessage")?
                }
                _ = parent_cancellation_token.cancelled() => return Ok(()),
                _ = session_cancellation_token.cancelled() => return Ok(()),
            };
            if fds.len() != msg.fds.len() {
                return Err(anyhow::anyhow!(
                    "mismatched number of fds in SuperExecMessage: {} in the message, {} from the control message",
                    msg.fds.len(),
                    fds.len()
                ));
            }

            let PreparedExec {
                command,
                cwd,
                env,
                arg0,
            } = tokio::select! {
                prepared = command_executor.prepare_escalated_exec(&program, &argv, &workdir, env, execution) => prepared?,
                _ = parent_cancellation_token.cancelled() => return Ok(()),
                _ = session_cancellation_token.cancelled() => return Ok(()),
            };
            let (program, args) = command
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("prepared escalated command must not be empty"))?;
            let mut command = Command::new(program);
            command
                .args(args)
                .arg0(arg0.unwrap_or_else(|| program.clone()))
                .envs(&env)
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            unsafe {
                command.pre_exec(move || {
                    for (dst_fd, src_fd) in msg.fds.iter().zip(&fds) {
                        libc::dup2(src_fd.as_raw_fd(), *dst_fd);
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn()?;
            let exit_status = tokio::select! {
                status = child.wait() => status?,
                _ = parent_cancellation_token.cancelled() => {
                    let _ = child.start_kill();
                    child.wait().await?
                }
                _ = session_cancellation_token.cancelled() => {
                    let _ = child.start_kill();
                    child.wait().await?
                }
            };
            socket
                .send(SuperExecResult {
                    exit_code: exit_status.code().unwrap_or(127),
                })
                .await?;
        }
        EscalationDecision::Deny { reason } => {
            socket
                .send(EscalateResponse {
                    action: EscalateAction::Deny { reason },
                })
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
