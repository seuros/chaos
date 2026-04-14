//! Claude Code subprocess transport.
//!
//! Manages the lifecycle of a `claude` subprocess, speaks the stream-json
//! control protocol over stdin/stdout, and handles bidirectional message
//! routing.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use futures::future::BoxFuture;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::io::BufWriter;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::protocol::ControlResponse;
use crate::protocol::Message;
use crate::protocol::UserMessage;
use crate::protocol::control_request_envelope;
use crate::protocol::initialize_request;

/// Async callback for Claude Code `can_use_tool` requests.
pub type ToolPermissionHandler = Arc<
    dyn Fn(String, Value, Option<String>) -> BoxFuture<'static, Result<Value, String>>
        + Send
        + Sync,
>;

/// Async callback for Claude Code `hook_callback` requests.
pub type HookCallbackHandler = Arc<
    dyn Fn(String, Value, Option<String>) -> BoxFuture<'static, Result<Value, String>>
        + Send
        + Sync,
>;

/// Async callback for Claude Code `mcp_message` requests.
pub type McpMessageHandler =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

/// Configuration for spawning the Claude Code subprocess.
#[derive(Clone)]
pub struct ClampConfig {
    /// Path to the `claude` binary. If None, we search PATH.
    pub cli_path: Option<PathBuf>,
    /// Whether to launch Claude Code in bare mode.
    pub bare_mode: bool,
    /// Working directory for the subprocess.
    pub cwd: Option<PathBuf>,
    /// System prompt to send (empty string = blank).
    pub system_prompt: Option<String>,
    /// MCP server config JSON to pass via --mcp-config.
    pub mcp_config: Option<Value>,
    /// Permission mode (e.g., "dontAsk").
    pub permission_mode: Option<String>,
    /// Tools to disallow (stripped from Claude Code).
    pub disallowed_tools: Vec<String>,
    /// Additional allowed tools.
    pub allowed_tools: Vec<String>,
    /// Whether Claude Code's built-in tools may execute directly.
    pub allow_claude_code_tools: bool,
    /// Optional handler for Claude Code permission requests.
    pub tool_permission_handler: Option<ToolPermissionHandler>,
    /// Optional handler for Claude Code hook callbacks.
    pub hook_callback_handler: Option<HookCallbackHandler>,
    /// Optional handler for Claude Code MCP messages.
    pub mcp_message_handler: Option<McpMessageHandler>,
}

impl std::fmt::Debug for ClampConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClampConfig")
            .field("cli_path", &self.cli_path)
            .field("bare_mode", &self.bare_mode)
            .field("cwd", &self.cwd)
            .field("system_prompt", &self.system_prompt)
            .field("mcp_config", &self.mcp_config)
            .field("permission_mode", &self.permission_mode)
            .field("disallowed_tools", &self.disallowed_tools)
            .field("allowed_tools", &self.allowed_tools)
            .field("allow_claude_code_tools", &self.allow_claude_code_tools)
            .field(
                "tool_permission_handler",
                &self.tool_permission_handler.as_ref().map(|_| "<handler>"),
            )
            .field(
                "hook_callback_handler",
                &self.hook_callback_handler.as_ref().map(|_| "<handler>"),
            )
            .field(
                "mcp_message_handler",
                &self.mcp_message_handler.as_ref().map(|_| "<handler>"),
            )
            .finish()
    }
}

impl Default for ClampConfig {
    fn default() -> Self {
        Self {
            cli_path: None,
            bare_mode: false,
            cwd: None,
            system_prompt: Some(String::new()),
            mcp_config: None,
            permission_mode: Some("default".to_string()),
            disallowed_tools: vec![],
            allowed_tools: vec![],
            allow_claude_code_tools: false,
            tool_permission_handler: None,
            hook_callback_handler: None,
            mcp_message_handler: None,
        }
    }
}

/// Pending control request tracker.
struct PendingRequest {
    tx: oneshot::Sender<Result<Value, String>>,
}

/// The Claude Code subprocess transport.
///
/// Drives Claude Code via the stream-json control protocol.
/// The transport handles:
/// - Spawning the subprocess with correct flags
/// - The initialization handshake
/// - Sending user messages and control requests
/// - Receiving and routing messages from stdout
/// - Responding to Claude Code's control requests (hooks, permissions, MCP)
pub struct ClampTransport {
    /// Write end to the subprocess stdin.
    stdin: BufWriter<ChildStdin>,
    /// Channel receiving parsed messages from the reader task.
    message_rx: mpsc::Receiver<Message>,
    /// Pending control requests awaiting responses.
    pending: HashMap<String, PendingRequest>,
    /// Non-control messages received while waiting on a control response.
    queued_messages: VecDeque<Message>,
    /// Request ID counter.
    request_counter: AtomicU64,
    /// The child process handle.
    child: Child,
    /// Whether the transport has been initialized.
    initialized: bool,
    /// When the subprocess was spawned.
    spawned_at: std::time::Instant,
    /// The initialization response from Claude Code (contains models, commands, etc.).
    init_response: Option<Value>,
    /// Current Claude Code session ID.
    session_id: String,
    /// Whether Claude Code may use its own built-in tools directly.
    allow_claude_code_tools: bool,
    /// Optional handler for permission requests.
    tool_permission_handler: Option<ToolPermissionHandler>,
    /// Optional handler for hook callbacks.
    hook_callback_handler: Option<HookCallbackHandler>,
    /// Optional handler for MCP messages.
    mcp_message_handler: Option<McpMessageHandler>,
}

/// Runtime info about the clamped Claude Code subprocess.
#[derive(Debug, Clone)]
pub struct ClampInfo {
    /// OS process ID of the claude subprocess.
    pub pid: u32,
    /// How long the subprocess has been running.
    pub uptime: std::time::Duration,
}

impl std::fmt::Debug for ClampTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClampTransport")
            .field("initialized", &self.initialized)
            .finish()
    }
}

/// Find the `claude` CLI binary.
fn find_claude_cli(config: &ClampConfig) -> Result<PathBuf, ClampError> {
    if let Some(path) = &config.cli_path {
        if path.exists() {
            return Ok(path.clone());
        }
        return Err(ClampError::CliNotFound(format!(
            "specified path does not exist: {}",
            path.display()
        )));
    }

    which::which("claude").map_err(|_| {
        ClampError::CliNotFound(
            "claude not found in PATH — install with: npm install -g @anthropic-ai/claude-code"
                .to_string(),
        )
    })
}

/// Build the command-line arguments for the Claude Code subprocess.
fn build_command(cli_path: &PathBuf, config: &ClampConfig) -> Command {
    let mut cmd = Command::new(cli_path);

    // Core stream-json flags (matching the SDK's SubprocessCLITransport)
    cmd.args(["--output-format", "stream-json"]);
    cmd.arg("--verbose");
    cmd.args(["--input-format", "stream-json"]);
    if config.bare_mode {
        cmd.arg("--bare");
    }

    // Skip all setting discovery — we provide everything explicitly.
    cmd.args(["--setting-sources", ""]);

    // System prompt
    match &config.system_prompt {
        Some(prompt) => {
            cmd.args(["--system-prompt", prompt]);
        }
        None => {
            cmd.args(["--system-prompt", ""]);
        }
    }

    // Permission mode
    if let Some(mode) = &config.permission_mode {
        cmd.args(["--permission-mode", mode]);
    }

    // MCP config
    if let Some(mcp) = &config.mcp_config {
        cmd.args(["--mcp-config", &mcp.to_string()]);
    }

    // Tool exposure: when allow_claude_code_tools is false (the default),
    // disable all built-in tools so Claude Code only sees the MCP bridge.
    // When true, leave the tool set unrestricted for direct use.
    if !config.allow_claude_code_tools {
        cmd.args(["--tools", ""]);
    } else {
        if !config.disallowed_tools.is_empty() {
            cmd.args(["--disallowedTools", &config.disallowed_tools.join(",")]);
        }
        if !config.allowed_tools.is_empty() {
            cmd.args(["--allowedTools", &config.allowed_tools.join(",")]);
        }
    }

    // Working directory
    if let Some(cwd) = &config.cwd {
        cmd.current_dir(cwd);
    }

    // Stdin/stdout are piped for the control protocol
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // Environment: identify ourselves as an SDK client
    cmd.env("CLAUDE_CODE_ENTRYPOINT", "sdk-chaos");

    cmd
}

/// Errors from the clamp transport.
#[derive(Debug, thiserror::Error)]
pub enum ClampError {
    #[error("Claude CLI not found: {0}")]
    CliNotFound(String),

    #[error("failed to spawn claude subprocess: {0}")]
    SpawnFailed(#[from] std::io::Error),

    #[error("control request timed out: {0}")]
    Timeout(String),

    #[error("control request failed: {0}")]
    ControlError(String),

    #[error("transport closed")]
    Closed,

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl ClampTransport {
    /// Spawn a Claude Code subprocess and set up the transport.
    pub async fn spawn(config: ClampConfig) -> Result<Self, ClampError> {
        let cli_path = find_claude_cli(&config)?;
        info!(cli = %cli_path.display(), "spawning claude subprocess for clamping");

        let mut cmd = build_command(&cli_path, &config);
        let mut child = cmd.spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClampError::Protocol("no stdin on child".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClampError::Protocol("no stdout on child".to_string()))?;

        // Drain stderr in background
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!(target: "chaos_clamp::stderr", "{}", line);
                }
            });
        }

        // Spawn a reader task that parses stdout line by line
        let (msg_tx, msg_rx) = mpsc::channel::<Message>(64);
        tokio::spawn(read_stdout(stdout, msg_tx));

        Ok(Self {
            stdin: BufWriter::new(stdin),
            message_rx: msg_rx,
            pending: HashMap::new(),
            queued_messages: VecDeque::new(),
            request_counter: AtomicU64::new(0),
            child,
            initialized: false,
            spawned_at: std::time::Instant::now(),
            init_response: None,
            session_id: "default".to_string(),
            allow_claude_code_tools: config.allow_claude_code_tools,
            tool_permission_handler: config.tool_permission_handler,
            hook_callback_handler: config.hook_callback_handler,
            mcp_message_handler: config.mcp_message_handler,
        })
    }

    /// Run the initialization handshake.
    ///
    /// This pumps the message loop internally while waiting for the
    /// control response, since no external caller is driving `next_message()`
    /// yet during initialization.
    pub async fn initialize(&mut self) -> Result<Value, ClampError> {
        let id = self.next_request_id();
        let envelope = control_request_envelope(&id, initialize_request());
        self.write_json(&envelope).await?;

        // Pump messages until we get the initialize response.
        // Any non-control messages received during init are discarded.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let msg = tokio::time::timeout_at(deadline, self.message_rx.recv())
                .await
                .map_err(|_| ClampError::Timeout(id.clone()))?
                .ok_or(ClampError::Closed)?;

            let Some(resp_id) = control_response_request_id(&msg) else {
                if let Some(queued) = self.handle_message(msg).await? {
                    self.queued_messages.push_back(queued);
                }
                continue;
            };

            if resp_id != id {
                if let Some(queued) = self.handle_message(msg).await? {
                    self.queued_messages.push_back(queued);
                }
                continue;
            }

            let Message::ControlResponse { response } = msg else {
                continue;
            };
            let subtype = response
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if subtype == "error" {
                let err = response
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                return Err(ClampError::ControlError(err));
            }
            let result = response
                .get("response")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            if let Some(models) = result.get("models").cloned() {
                crate::set_cached_models(models);
            }
            self.initialized = true;
            self.init_response = Some(result.clone());
            info!("claude subprocess initialized");
            return Ok(result);
        }
    }

    /// Get the initialization response (contains models, commands, agents, etc.).
    pub fn init_response(&self) -> Option<&Value> {
        self.init_response.as_ref()
    }

    /// Get runtime info about the subprocess.
    pub fn info(&self) -> Option<ClampInfo> {
        self.child.id().map(|pid| ClampInfo {
            pid,
            uptime: self.spawned_at.elapsed(),
        })
    }

    /// Switch the model on the running Claude Code subprocess.
    pub async fn set_model(&mut self, model: &str) -> Result<Value, ClampError> {
        self.send_control_request(serde_json::json!({
            "subtype": "set_model",
            "model": model
        }))
        .await
    }

    /// Send a user message (prompt) to Claude Code.
    pub async fn send_user_message(&mut self, content: &str) -> Result<(), ClampError> {
        let msg = UserMessage::new(content.to_string(), self.session_id.clone());
        self.write_json(&serde_json::to_value(msg)?).await
    }

    /// Send a control request and wait for the response.
    pub async fn send_control_request(&mut self, request: Value) -> Result<Value, ClampError> {
        let id = self.next_request_id();
        let envelope = control_request_envelope(&id, request);
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id.clone(), PendingRequest { tx });
        self.write_json(&envelope).await?;
        let mut rx = rx;
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(60));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                result = &mut rx => {
                    let result = result.map_err(|_| ClampError::Closed)?;
                    return result.map_err(ClampError::ControlError);
                }
                _ = &mut timeout => {
                    self.pending.remove(&id);
                    return Err(ClampError::Timeout(id));
                }
                maybe_msg = self.message_rx.recv() => {
                    let msg = maybe_msg.ok_or(ClampError::Closed)?;
                    if let Some(queued) = self.handle_message(msg).await? {
                        self.queued_messages.push_back(queued);
                    }
                }
            }
        }
    }

    /// Read the next message from Claude Code, handling control messages internally.
    ///
    /// This is the main message pump. It:
    /// 1. Reads messages from the subprocess stdout
    /// 2. Routes control responses to pending requests
    /// 3. Handles incoming control requests (hooks, permissions, MCP)
    /// 4. Returns assistant/result/system messages to the caller
    pub async fn next_message(&mut self) -> Result<Option<Message>, ClampError> {
        loop {
            if let Some(msg) = self.queued_messages.pop_front() {
                return Ok(Some(msg));
            }

            let msg = match self.message_rx.recv().await {
                Some(msg) => msg,
                None => return Ok(None),
            };

            if let Some(message) = self.handle_message(msg).await? {
                return Ok(Some(message));
            }
        }
    }

    /// Shut down the subprocess gracefully.
    pub async fn shutdown(mut self) -> Result<(), ClampError> {
        // Close stdin to signal EOF
        drop(self.stdin);

        // Wait for graceful exit
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await;

        match result {
            Ok(Ok(status)) => {
                info!(?status, "claude subprocess exited");
            }
            Ok(Err(e)) => {
                warn!("error waiting for claude subprocess: {e}");
            }
            Err(_) => {
                warn!("claude subprocess did not exit in time, killing");
                let _ = self.child.kill().await;
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn next_request_id(&self) -> String {
        let n = self.request_counter.fetch_add(1, Ordering::Relaxed);
        format!("chaos_req_{n}")
    }

    async fn write_json(&mut self, value: &Value) -> Result<(), ClampError> {
        let line = serde_json::to_string(value)?;
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ClampError::Protocol(format!("stdin write failed: {e}")))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| ClampError::Protocol(format!("stdin write failed: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| ClampError::Protocol(format!("stdin flush failed: {e}")))?;
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message) -> Result<Option<Message>, ClampError> {
        match msg {
            Message::ControlResponse { response } => {
                self.route_control_response(response);
                Ok(None)
            }
            Message::ControlRequestIncoming {
                request_id,
                request,
            } => {
                self.handle_incoming_control_request(request_id, request)
                    .await?;
                Ok(None)
            }
            Message::ControlCancelRequest { request_id } => {
                debug!(request_id, "control request cancelled");
                self.pending.remove(&request_id);
                Ok(None)
            }
            Message::Result {
                session_id: Some(session_id),
                result,
                total_cost_usd,
            } => {
                self.session_id = session_id.clone();
                Ok(Some(Message::Result {
                    result,
                    total_cost_usd,
                    session_id: Some(session_id),
                }))
            }
            msg @ (Message::Assistant { .. } | Message::Result { .. } | Message::System { .. }) => {
                Ok(Some(msg))
            }
            Message::Unknown => Ok(None),
        }
    }

    fn route_control_response(&mut self, response: Value) {
        let request_id = response
            .get("request_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if let Some(pending) = self.pending.remove(&request_id) {
            let subtype = response
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let result = if subtype == "error" {
                let err = response
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                Err(err)
            } else {
                Ok(response
                    .get("response")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default())))
            };

            let _ = pending.tx.send(result);
        }
    }

    async fn handle_incoming_control_request(
        &mut self,
        request_id: String,
        request: Value,
    ) -> Result<(), ClampError> {
        let subtype = request
            .get("subtype")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        debug!(subtype, request_id, "incoming control request from claude");

        match subtype {
            "hook_callback" => {
                let callback_id = request
                    .get("callback_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let input = request.get("input").cloned().unwrap_or(Value::Null);
                let tool_use_id = request
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned);
                let payload = if let Some(handler) = &self.hook_callback_handler {
                    handler(callback_id, input, tool_use_id)
                        .await
                        .map_err(ClampError::ControlError)?
                } else {
                    serde_json::json!({})
                };
                let response = ControlResponse::success(request_id, payload);
                self.write_json(&serde_json::to_value(response)?).await?;
            }
            "can_use_tool" => {
                let tool_name = request
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let input = request.get("input").cloned().unwrap_or(Value::Null);
                let tool_use_id = request
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned);
                let payload = if let Some(handler) = &self.tool_permission_handler {
                    handler(tool_name.to_string(), input.clone(), tool_use_id)
                        .await
                        .map_err(ClampError::ControlError)?
                } else if self.allow_claude_code_tools {
                    debug!(tool_name, "allowing Claude Code built-in tool use");
                    default_tool_permission_response(/*allow*/ true, input, None)
                } else {
                    debug!(tool_name, "denying Claude Code built-in tool use");
                    default_tool_permission_response(
                        /*allow*/ false,
                        input,
                        Some("Claude Code built-in tools are disabled in clamp mode; use Chaos-managed tools instead.".to_string()),
                    )
                };
                let response = ControlResponse::success(request_id, payload);
                self.write_json(&serde_json::to_value(response)?).await?;
            }
            "mcp_message" => {
                let server_name = request
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let mcp_message = request.get("message").cloned().unwrap_or(Value::Null);

                debug!(server_name, "MCP message from claude");

                let mcp_response = if let Some(handler) = &self.mcp_message_handler {
                    handler(server_name.to_string(), mcp_message.clone())
                        .await
                        .map_err(ClampError::ControlError)?
                } else {
                    default_mcp_error_response(server_name, &mcp_message)
                };

                let response = ControlResponse::success(
                    request_id,
                    serde_json::json!({"mcp_response": mcp_response}),
                );
                self.write_json(&serde_json::to_value(response)?).await?;
            }
            _ => {
                warn!(subtype, "unhandled control request subtype");
                let response = ControlResponse::error(
                    request_id,
                    format!("unsupported control request: {subtype}"),
                );
                self.write_json(&serde_json::to_value(response)?).await?;
            }
        }

        Ok(())
    }
}

fn control_response_request_id(msg: &Message) -> Option<&str> {
    let Message::ControlResponse { response } = msg else {
        return None;
    };
    response.get("request_id").and_then(|v| v.as_str())
}

fn default_tool_permission_response(
    allow: bool,
    input: Value,
    deny_message: Option<String>,
) -> Value {
    if allow {
        serde_json::json!({
            "behavior": "allow",
            "updatedInput": input
        })
    } else {
        serde_json::json!({
            "behavior": "deny",
            "message": deny_message.unwrap_or_else(|| "tool use denied".to_string())
        })
    }
}

fn default_mcp_error_response(server_name: &str, request_message: &Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_message.get("id"),
        "error": {
            "code": -32601,
            "message": format!("MCP routing not yet implemented for server '{server_name}'")
        }
    })
}

/// Background task: read stdout line-by-line, parse JSON, send to channel.
async fn read_stdout(stdout: ChildStdout, tx: mpsc::Sender<Message>) {
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut json_buffer = String::new();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // EOF
            Err(e) => {
                error!("stdout read error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip non-JSON lines when not mid-buffer
        if json_buffer.is_empty() && !trimmed.starts_with('{') {
            debug!(
                "skipping non-JSON stdout line: {}",
                &trimmed[..trimmed.len().min(200)]
            );
            continue;
        }

        json_buffer.push_str(trimmed);

        // Try to parse — Claude Code may split long JSON across lines
        match serde_json::from_str::<Value>(&json_buffer) {
            Ok(value) => {
                json_buffer.clear();

                // Parse as a typed Message
                let msg = match serde_json::from_value::<Message>(value.clone()) {
                    Ok(msg) => msg,
                    Err(_) => {
                        debug!("unrecognized message type: {:?}", value.get("type"));
                        Message::Unknown
                    }
                };

                if tx.send(msg).await.is_err() {
                    break; // Receiver dropped
                }
            }
            Err(_) => {
                // Incomplete JSON, keep buffering
                if json_buffer.len() > 1_048_576 {
                    error!("JSON buffer exceeded 1MB, dropping");
                    json_buffer.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tool_permission_allow_preserves_input() {
        let input = serde_json::json!({"command": "ls"});
        let value = default_tool_permission_response(true, input.clone(), None);
        assert_eq!(value["behavior"], "allow");
        assert_eq!(value["updatedInput"], input);
    }

    #[test]
    fn default_tool_permission_deny_sets_message() {
        let value = default_tool_permission_response(false, Value::Null, Some("nope".to_string()));
        assert_eq!(value["behavior"], "deny");
        assert_eq!(value["message"], "nope");
    }

    #[test]
    fn default_mcp_error_reuses_jsonrpc_id() {
        let request = serde_json::json!({"id": 42, "method": "tools/call"});
        let response = default_mcp_error_response("chaos", &request);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 42);
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn control_response_request_id_extracts_id() {
        let msg = Message::ControlResponse {
            response: serde_json::json!({
                "request_id": "req_123",
                "subtype": "success"
            }),
        };
        assert_eq!(control_response_request_id(&msg), Some("req_123"));
    }

    #[test]
    fn build_command_no_bare_mode_by_default() {
        // bare_mode is false by default; keychain auth must remain accessible
        // for Claude Code MAX OAuth to work.
        let config = ClampConfig::default();
        let command = build_command(&PathBuf::from("claude"), &config);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|arg| arg == "--bare"));
        // settings files are still blocked
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--setting-sources" && w[1].is_empty())
        );
    }

    #[test]
    fn build_command_includes_disallowed_tools() {
        let config = ClampConfig {
            disallowed_tools: vec!["Bash".to_string(), "Read".to_string()],
            // disallowed_tools only takes effect when CC tools are permitted
            allow_claude_code_tools: true,
            ..Default::default()
        };
        let command = build_command(&PathBuf::from("claude"), &config);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2)
                .any(|window| { window[0] == "--disallowedTools" && window[1] == "Bash,Read" })
        );
    }
}
