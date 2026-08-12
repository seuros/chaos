//! Google Antigravity CLI subprocess transport.
//!
//! This transport deliberately leaves Google credentials in `agy`'s private
//! home directory. It invokes one non-interactive process per turn, parses the
//! documented stream-JSON output as it arrives, and resumes later turns by
//! provider conversation ID.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::sync::mpsc;

const DEFAULT_PRINT_TIMEOUT: Duration = Duration::from_secs(300);
const STDERR_CLASSIFICATION_LIMIT: usize = 16 * 1024;
/// Extra wall-clock allowance beyond the CLI's own `--print-timeout`, so the
/// subprocess reports its own deadline before this transport kills it.
const PRINT_TIMEOUT_GRACE: Duration = Duration::from_secs(30);
/// Upper bound on stream-JSON accepted from one invocation.
const STDOUT_LIMIT: u64 = 16 * 1024 * 1024;
const MAX_CONVERSATION_ID_LEN: usize = 256;
const BRIDGE_SOCKET_ENV: &str = "CHAOS_CLAMP_MCP_SOCKET";
const BRIDGE_TOKEN_ENV: &str = "CHAOS_CLAMP_MCP_TOKEN";
const CHAOS_MCP_SERVER_NAME: &str = "chaos";
const CHAOS_MCP_ALLOW_RULE: &str = "mcp(chaos/*)";
const NATIVE_TOOL_DENY_RULES: &[&str] = &[
    "command(*)",
    "unsandboxed(*)",
    "read_file(*)",
    "write_file(*)",
    "read_url(*)",
    "execute_url(*)",
];

/// Ephemeral Chaos bridge capability inherited by `agy` and its MCP child.
///
/// The socket path and token are intentionally exported only through the
/// subprocess environment. Managed Antigravity configuration contains neither.
#[derive(Debug, Clone)]
pub struct AntigravityBridgeConfig {
    pub socket_path: PathBuf,
    pub token: String,
    pub chaos_executable: PathBuf,
}

/// Configuration for invoking the official `agy` CLI.
#[derive(Debug, Clone)]
pub struct AntigravityConfig {
    /// Path to `agy`. When omitted, the binary is resolved from `PATH`.
    pub cli_path: Option<PathBuf>,
    /// Isolated home directory containing state owned by `agy`.
    pub home: Option<PathBuf>,
    /// Working directory presented to `agy`.
    pub cwd: Option<PathBuf>,
    /// Antigravity model slug, for example `gemini-3.1-pro-low`.
    pub model: String,
    /// Optional Antigravity reasoning effort (`low`, `medium`, or `high`).
    pub effort: Option<String>,
    /// Maximum wall-clock time for a single print-mode invocation.
    pub print_timeout: Duration,
    /// Session-scoped Chaos MCP bridge. Required for a usable clamped turn.
    pub bridge: Option<AntigravityBridgeConfig>,
    /// Kernel sandbox wrapper the CLI is executed through. Without it the CLI
    /// is only as contained as it chooses to be.
    pub sandbox: Option<AntigravitySandbox>,
    /// Loopback egress proxy the CLI must route through.
    pub egress: Option<AntigravityEgress>,
}

/// A helper the CLI is exec'd through so the operating system, not the CLI,
/// enforces containment. `program` is the platform sandbox helper (for example
/// `alcatraz-linux`) and `args` are its policy arguments, terminated by `--`.
#[derive(Debug, Clone)]
pub struct AntigravitySandbox {
    /// Sandbox helper executable.
    pub program: PathBuf,
    /// Name the helper must see as `argv[0]`. Multicall builds dispatch on it,
    /// so launching the same file under its own path selects the wrong tool.
    pub arg0: Option<String>,
    /// Helper arguments, ending with the `--` separator.
    pub args: Vec<String>,
}

/// Where the CLI's outbound HTTP must go, and which trust root makes the
/// interposed TLS session verify.
#[derive(Debug, Clone)]
pub struct AntigravityEgress {
    /// Proxy URL exported as `HTTPS_PROXY`/`HTTP_PROXY`.
    pub proxy_url: String,
    /// Session CA bundle exported as `SSL_CERT_FILE`, when TLS is interposed.
    pub ca_bundle_path: Option<PathBuf>,
}

impl Default for AntigravityConfig {
    fn default() -> Self {
        Self {
            cli_path: None,
            home: None,
            cwd: None,
            model: "gemini-3.1-pro-low".to_string(),
            effort: None,
            print_timeout: DEFAULT_PRINT_TIMEOUT,
            bridge: None,
            sandbox: None,
            egress: None,
        }
    }
}

/// Token usage reported by Antigravity for a step or complete invocation.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct AntigravityUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub thinking_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// Initialization metadata emitted at the beginning of every invocation.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct AntigravityInit {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
}

/// A streamed Antigravity step update.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AntigravityStepUpdate {
    pub conversation_id: String,
    pub step_index: u64,
    pub state: String,
    pub step_type: String,
    #[serde(default)]
    pub text_delta: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub usage: Option<AntigravityUsage>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_info: Option<Value>,
}

/// Final result emitted by a successful or failed Antigravity invocation.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AntigravityResult {
    pub conversation_id: String,
    pub status: String,
    #[serde(default)]
    pub response: String,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub num_turns: Option<u64>,
    #[serde(default)]
    pub usage: Option<AntigravityUsage>,
}

/// Parsed `agy --output-format stream-json` event.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AntigravityEvent {
    Init {
        conversation_id: String,
        init: AntigravityInit,
    },
    StepUpdate {
        step_update: AntigravityStepUpdate,
    },
    Result {
        result: AntigravityResult,
    },
}

/// Completed turn returned by [`AntigravityTransport`].
#[derive(Debug, Clone)]
pub struct AntigravityTurn {
    pub conversation_id: String,
    pub response: String,
    pub usage: Option<AntigravityUsage>,
    pub events: Vec<AntigravityEvent>,
}

/// Errors from the Antigravity subprocess transport.
#[derive(Debug, thiserror::Error)]
pub enum AntigravityError {
    #[error("Antigravity CLI not found: {0}")]
    CliNotFound(String),

    #[error("failed to run Antigravity CLI: {0}")]
    Spawn(std::io::Error),

    #[error("Antigravity state I/O failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("Antigravity invocation timed out")]
    Timeout,

    #[error("Antigravity subscription authentication is unavailable")]
    AuthenticationUnavailable,

    #[error("Antigravity invocation failed with status {0}")]
    InvocationFailed(String),

    #[error("Antigravity protocol error: {0}")]
    Protocol(String),
}

/// Session-scoped Antigravity transport.
///
/// `agy` itself is turn-scoped; this type persists only the provider
/// conversation ID needed to resume the next subprocess invocation.
#[derive(Debug)]
pub struct AntigravityTransport {
    config: AntigravityConfig,
    conversation_id: Option<String>,
    managed_home_prepared: bool,
}

impl AntigravityTransport {
    pub fn new(config: AntigravityConfig) -> Result<Self, AntigravityError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            conversation_id: None,
            managed_home_prepared: false,
        })
    }

    pub fn with_conversation_id(
        config: AntigravityConfig,
        conversation_id: String,
    ) -> Result<Self, AntigravityError> {
        validate_config(&config)?;
        if !is_safe_conversation_id(&conversation_id) {
            return Err(AntigravityError::Protocol(
                "refusing to resume an unsafe Antigravity conversation id".to_string(),
            ));
        }
        Ok(Self {
            config,
            conversation_id: Some(conversation_id),
            managed_home_prepared: false,
        })
    }

    pub fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Run one prompt through `agy`, resuming the provider conversation when
    /// this transport has already completed a turn.
    pub async fn run_turn(&mut self, prompt: &str) -> Result<AntigravityTurn, AntigravityError> {
        self.run_turn_streamed(prompt, None).await
    }

    /// Run one prompt through `agy`, forwarding every parsed event to `sink`
    /// as the subprocess emits it.
    pub async fn run_turn_streamed(
        &mut self,
        prompt: &str,
        sink: Option<&mpsc::Sender<AntigravityEvent>>,
    ) -> Result<AntigravityTurn, AntigravityError> {
        let cli_path = find_agy_cli(&self.config)?;
        if !self.managed_home_prepared {
            prepare_managed_home(&self.config)?;
            self.managed_home_prepared = true;
        }
        let mut command = build_command(&cli_path, &self.config, self.conversation_id.as_deref());
        command.arg("--print").arg(prompt);

        let mut child = command.spawn().map_err(AntigravityError::Spawn)?;
        let deadline = self.config.print_timeout + PRINT_TIMEOUT_GRACE;
        let invocation =
            match tokio::time::timeout(deadline, drive_invocation(&mut child, sink)).await {
                Ok(invocation) => invocation?,
                Err(_) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(AntigravityError::Timeout);
                }
            };

        if !invocation.status.success() {
            if text_indicates_auth_failure(&invocation.stderr_tail) {
                return Err(AntigravityError::AuthenticationUnavailable);
            }
            return Err(AntigravityError::InvocationFailed(
                invocation.status.code().map_or_else(
                    || "terminated by signal".to_string(),
                    |code| code.to_string(),
                ),
            ));
        }

        let events = invocation.events;
        let result = events.iter().rev().find_map(|event| match event {
            AntigravityEvent::Result { result } => Some(result),
            _ => None,
        });
        let Some(result) = result else {
            return Err(AntigravityError::Protocol(
                "stream ended without a result event".to_string(),
            ));
        };
        if result.status != "SUCCESS" {
            return Err(AntigravityError::InvocationFailed(result.status.clone()));
        }
        if let Some(expected) = self.conversation_id.as_deref()
            && result.conversation_id != expected
        {
            return Err(AntigravityError::Protocol(format!(
                "resumed conversation changed from {expected} to {}",
                result.conversation_id
            )));
        }
        if !is_safe_conversation_id(&result.conversation_id) {
            return Err(AntigravityError::Protocol(
                "Antigravity reported an unsafe conversation id".to_string(),
            ));
        }

        self.conversation_id = Some(result.conversation_id.clone());
        Ok(AntigravityTurn {
            conversation_id: result.conversation_id.clone(),
            response: result.response.clone(),
            usage: result.usage.clone(),
            events,
        })
    }
}

fn validate_config(config: &AntigravityConfig) -> Result<(), AntigravityError> {
    if config.model.trim().is_empty() {
        return Err(AntigravityError::Protocol(
            "Antigravity model must not be empty".to_string(),
        ));
    }
    if let Some(effort) = config.effort.as_deref()
        && !matches!(effort, "low" | "medium" | "high")
    {
        return Err(AntigravityError::Protocol(format!(
            "unsupported Antigravity effort: {effort}"
        )));
    }
    if config.home.is_none() {
        return Err(AntigravityError::Protocol(
            "Antigravity clamp requires a dedicated CHAOS_AGY_HOME".to_string(),
        ));
    }
    let Some(bridge) = config.bridge.as_ref() else {
        return Err(AntigravityError::Protocol(
            "Antigravity clamp requires the Chaos session MCP bridge".to_string(),
        ));
    };
    if bridge.token.is_empty() {
        return Err(AntigravityError::Protocol(
            "Antigravity clamp bridge token must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn find_agy_cli(config: &AntigravityConfig) -> Result<PathBuf, AntigravityError> {
    if let Some(path) = &config.cli_path {
        if !path.is_file() {
            return Err(AntigravityError::CliNotFound(format!(
                "specified path does not exist: {}",
                path.display()
            )));
        }
        if !is_executable(path) {
            return Err(AntigravityError::CliNotFound(format!(
                "specified path is not executable: {}",
                path.display()
            )));
        }
        return Ok(path.clone());
    }

    which::which("agy").map_err(|_| {
        AntigravityError::CliNotFound(
            "agy not found in PATH; install a pinned official Antigravity CLI artifact".to_string(),
        )
    })
}

fn build_command(
    cli_path: &Path,
    config: &AntigravityConfig,
    conversation_id: Option<&str>,
) -> Command {
    // When a sandbox helper is configured the CLI becomes its argument, so the
    // policy is applied by a process the CLI does not control before it ever
    // gets to run.
    let mut command = match &config.sandbox {
        Some(sandbox) => {
            let mut command = Command::new(&sandbox.program);
            if let Some(arg0) = &sandbox.arg0 {
                command.arg0(arg0);
            }
            command.args(&sandbox.args);
            command.arg(cli_path);
            command
        }
        None => Command::new(cli_path),
    };
    command.args(["--output-format", "stream-json"]);
    command.args(["--model", &config.model]);
    command.arg("--disable-slash-commands");
    command.arg("--sandbox");
    command.args([
        "--print-timeout",
        &format!("{}s", config.print_timeout.as_secs().max(1)),
    ]);

    if let Some(conversation_id) = conversation_id {
        command.args(["--conversation", conversation_id]);
    }
    if let Some(effort) = config.effort.as_deref() {
        command.args(["--effort", effort]);
    }
    if let Some(home) = &config.home {
        command.env("HOME", home);
        command.env("XDG_CONFIG_HOME", home.join(".config"));
    }
    if let Some(bridge) = &config.bridge {
        command.env(BRIDGE_SOCKET_ENV, &bridge.socket_path);
        command.env(BRIDGE_TOKEN_ENV, &bridge.token);
    }
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }

    if let Some(egress) = &config.egress {
        // Both are set because Go consults the scheme-specific variable first
        // and a CLI that falls back to plaintext must still land on the proxy.
        command.env("HTTPS_PROXY", &egress.proxy_url);
        command.env("HTTP_PROXY", &egress.proxy_url);
        command.env("https_proxy", &egress.proxy_url);
        command.env("http_proxy", &egress.proxy_url);
        // An inherited NO_PROXY would carve a hole straight through the policy.
        command.env_remove("NO_PROXY");
        command.env_remove("no_proxy");
        if let Some(ca_bundle) = &egress.ca_bundle_path {
            command.env("SSL_CERT_FILE", ca_bundle);
        }
    }

    // Subscription mode must never silently fall back to a metered API key.
    command.env_remove("GEMINI_API_KEY");
    command.env_remove("GOOGLE_API_KEY");
    command.kill_on_drop(true);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command
}

fn prepare_managed_home(config: &AntigravityConfig) -> Result<(), AntigravityError> {
    let home = config.home.as_ref().ok_or_else(|| {
        AntigravityError::Protocol(
            "Antigravity clamp requires a dedicated CHAOS_AGY_HOME".to_string(),
        )
    })?;
    let bridge = config.bridge.as_ref().ok_or_else(|| {
        AntigravityError::Protocol(
            "Antigravity clamp requires the Chaos session MCP bridge".to_string(),
        )
    })?;

    let mcp_path = home.join(".gemini/config/mcp_config.json");
    let mut mcp_config = read_json_object_or_empty(&mcp_path)?;
    mcp_config["mcpServers"] = serde_json::json!({
        CHAOS_MCP_SERVER_NAME: {
            "command": bridge.chaos_executable,
            "args": ["clamp-session-bridge"]
        }
    });
    atomic_write_private_json(&mcp_path, &mcp_config)?;

    let settings_path = home.join(".gemini/antigravity-cli/settings.json");
    let mut settings = read_json_object_or_empty(&settings_path)?;
    settings["permissions"] = serde_json::json!({
        "allow": [CHAOS_MCP_ALLOW_RULE],
        "deny": NATIVE_TOOL_DENY_RULES
    });
    atomic_write_private_json(&settings_path, &settings)?;
    Ok(())
}

fn read_json_object_or_empty(path: &Path) -> Result<Value, AntigravityError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                AntigravityError::Protocol(format!(
                    "invalid managed Antigravity configuration {}: {error}",
                    path.display()
                ))
            })?;
            if value.is_object() {
                Ok(value)
            } else {
                Err(AntigravityError::Protocol(format!(
                    "managed Antigravity configuration is not an object: {}",
                    path.display()
                )))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(Default::default()))
        }
        Err(error) => Err(AntigravityError::Io(error)),
    }
}

fn atomic_write_private_json(path: &Path, value: &Value) -> Result<(), AntigravityError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        AntigravityError::Protocol(format!("failed to encode {}: {error}", path.display()))
    })?;
    atomic_write_private(path, &bytes)
}

/// Writes `bytes` to `path` through a private temporary file in the same
/// directory, so a reader never observes a half-written state file and the
/// contents are never world-readable.
pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), AntigravityError> {
    let parent = path.parent().ok_or_else(|| {
        AntigravityError::Protocol(format!("path has no parent directory: {}", path.display()))
    })?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }

    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary_path = parent.join(format!(
        ".{}.{}.{nonce}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("managed-config"),
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&temporary_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = std::fs::rename(&temporary_path, path) {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(AntigravityError::Io(error));
    }
    Ok(())
}

/// Provider conversation id persisted across Chaos processes, so
/// `chaos exec resume` continues the same Antigravity conversation.
#[derive(Debug, Clone)]
pub struct AntigravityConversationStore {
    path: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedConversation {
    version: u8,
    model: String,
    conversation_id: String,
}

impl AntigravityConversationStore {
    const VERSION: u8 = 1;

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the persisted conversation id when it was recorded for `model`
    /// by a compatible version, and is safe to pass back to `agy`.
    pub fn load(&self, model: &str) -> Option<String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    "failed to read persisted Antigravity conversation state: {error}"
                );
                return None;
            }
        };
        let state: PersistedConversation = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    "ignoring invalid persisted Antigravity conversation state: {error}"
                );
                return None;
            }
        };
        if state.version != Self::VERSION || state.model != model {
            return None;
        }
        let conversation_id = state.conversation_id.trim();
        if !is_safe_conversation_id(conversation_id) {
            tracing::warn!(
                path = %self.path.display(),
                "ignoring unsafe persisted Antigravity conversation id"
            );
            return None;
        }
        Some(conversation_id.to_string())
    }

    pub fn save(&self, model: &str, conversation_id: &str) -> Result<(), AntigravityError> {
        let state = PersistedConversation {
            version: Self::VERSION,
            model: model.to_string(),
            conversation_id: conversation_id.to_string(),
        };
        let bytes = serde_json::to_vec(&state).map_err(|error| {
            AntigravityError::Protocol(format!(
                "failed to encode Antigravity conversation state: {error}"
            ))
        })?;
        atomic_write_private(&self.path, &bytes)
    }

    pub fn clear(&self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                path = %self.path.display(),
                "failed to remove persisted Antigravity conversation state: {error}"
            ),
        }
    }
}

/// Conversation ids are interpolated into CLI arguments and file contents, so
/// only opaque identifier characters are accepted.
fn is_safe_conversation_id(conversation_id: &str) -> bool {
    !conversation_id.is_empty()
        && conversation_id.len() <= MAX_CONVERSATION_ID_LEN
        && conversation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Everything one `agy` invocation produced, once it has exited.
struct Invocation {
    events: Vec<AntigravityEvent>,
    status: std::process::ExitStatus,
    stderr_tail: String,
}

/// Reads stream-JSON events as `agy` emits them, drains stderr concurrently so
/// the subprocess cannot block on a full pipe, and waits for exit.
async fn drive_invocation(
    child: &mut tokio::process::Child,
    sink: Option<&mpsc::Sender<AntigravityEvent>>,
) -> Result<Invocation, AntigravityError> {
    let stdout = child.stdout.take().ok_or_else(|| {
        AntigravityError::Protocol("Antigravity stdout was not captured".to_string())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        AntigravityError::Protocol("Antigravity stderr was not captured".to_string())
    })?;

    let stderr_task = tokio::spawn(drain_stderr_tail(stderr));
    let events = read_events(stdout, sink).await;
    let status = child.wait().await.map_err(AntigravityError::Spawn)?;
    let stderr_tail = stderr_task.await.unwrap_or_default();

    // A nonzero exit explains a parse failure better than the parse failure
    // does, so surface the events error only for successful invocations.
    match events {
        Ok(events) => Ok(Invocation {
            events,
            status,
            stderr_tail,
        }),
        Err(error) if status.success() => Err(error),
        Err(_) => Ok(Invocation {
            events: Vec::new(),
            status,
            stderr_tail,
        }),
    }
}

async fn read_events<R: tokio::io::AsyncRead + Unpin>(
    stdout: R,
    sink: Option<&mpsc::Sender<AntigravityEvent>>,
) -> Result<Vec<AntigravityEvent>, AntigravityError> {
    let mut reader = BufReader::new(stdout.take(STDOUT_LIMIT + 1));
    let mut events = Vec::new();
    let mut line = String::new();
    let mut total = 0u64;
    let mut index = 0usize;
    let mut sink = sink;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(AntigravityError::Spawn)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > STDOUT_LIMIT {
            return Err(AntigravityError::Protocol(format!(
                "Antigravity emitted more than {STDOUT_LIMIT} bytes of stream JSON"
            )));
        }
        index += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: AntigravityEvent = serde_json::from_str(trimmed).map_err(|error| {
            AntigravityError::Protocol(format!("invalid JSONL event on line {index}: {error}"))
        })?;
        if let Some(sender) = sink
            && sender.send(event.clone()).await.is_err()
        {
            // The consumer dropped the turn. Keep draining so the subprocess
            // can exit on its own, but stop forwarding.
            sink = None;
        }
        events.push(event);
    }
    Ok(events)
}

async fn drain_stderr_tail(stderr: tokio::process::ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > 2 * STDERR_CLASSIFICATION_LIMIT {
                    let start = buffer.len() - STDERR_CLASSIFICATION_LIMIT;
                    buffer.drain(..start);
                }
            }
        }
    }
    let start = buffer.len().saturating_sub(STDERR_CLASSIFICATION_LIMIT);
    String::from_utf8_lossy(&buffer[start..]).into_owned()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Classifies a failed invocation's stderr tail. Markers are phrases rather
/// than bare words so ordinary prose mentioning a sign-in is not mistaken for
/// an expired subscription.
fn text_indicates_auth_failure(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "not logged in",
        "not authenticated",
        "unauthenticated",
        "authentication required",
        "authentication failed",
        "oauth token",
        "please sign in",
        "sign in to continue",
        "login required",
        "please log in",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_bridge(dir: &Path) -> AntigravityBridgeConfig {
        AntigravityBridgeConfig {
            socket_path: dir.join("bridge.sock"),
            token: "ephemeral-test-token".to_string(),
            chaos_executable: dir.join("chaos"),
        }
    }

    const OBSERVED_STREAM: &[u8] = br#"{"event":"init","conversation_id":"conversation-1","init":{"model":"gemini-3.1-pro-low","permission_mode":"request-review","tools":["run_command"]}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"hello\n","usage":{"input_tokens":11,"output_tokens":7,"thinking_tokens":5,"cache_read_tokens":3,"total_tokens":26}}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":4,"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"parameters":{"ServerName":"chaos","ToolName":"read_file"},"output":"ok"}}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":5,"state":"DONE","step_type":"future_checkpoint_kind"}}
{"event":"result","result":{"conversation_id":"conversation-1","status":"SUCCESS","response":"hello\n","duration_seconds":1.5,"num_turns":1,"usage":{"input_tokens":11,"output_tokens":7,"thinking_tokens":5,"cache_read_tokens":3,"total_tokens":26}}}
"#;

    /// One realistic stream covers init, text, tool, forward-compatible step
    /// kinds, and the terminal result — and proves each is forwarded to the
    /// sink before the invocation ends.
    #[tokio::test]
    async fn streams_observed_events_to_the_sink_as_they_parse() {
        let (tx, mut rx) = mpsc::channel(16);
        let events = read_events(OBSERVED_STREAM, Some(&tx))
            .await
            .expect("events should parse");
        drop(tx);

        let mut streamed = Vec::new();
        while let Some(event) = rx.recv().await {
            streamed.push(event);
        }
        assert_eq!(streamed, events);
        assert_eq!(events.len(), 5);

        let AntigravityEvent::StepUpdate { step_update } = &events[1] else {
            panic!("expected text step");
        };
        assert_eq!(step_update.text_delta.as_deref(), Some("hello\n"));
        let AntigravityEvent::StepUpdate { step_update } = &events[2] else {
            panic!("expected tool step");
        };
        assert_eq!(step_update.tool_name.as_deref(), Some("call_mcp_tool"));
        assert_eq!(
            step_update.tool_info.as_ref().expect("tool info")["output"],
            "ok"
        );
        let AntigravityEvent::StepUpdate { step_update } = &events[3] else {
            panic!("expected unknown step");
        };
        assert_eq!(step_update.step_type, "future_checkpoint_kind");
        let AntigravityEvent::Result { result } = &events[4] else {
            panic!("expected result event");
        };
        assert_eq!(result.response, "hello\n");
        assert_eq!(result.usage.as_ref().expect("usage").thinking_tokens, 5);
    }

    #[tokio::test]
    async fn rejects_stdout_beyond_the_stream_limit() {
        let oversized = vec![b'x'; usize::try_from(STDOUT_LIMIT).expect("limit fits usize") + 1];
        let error = read_events(oversized.as_slice(), None)
            .await
            .expect_err("oversized stdout should be rejected");
        assert!(matches!(error, AntigravityError::Protocol(_)), "{error}");
    }

    #[test]
    fn rejects_unknown_effort_and_unsafe_resume_ids() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let base = AntigravityConfig {
            home: Some(directory.path().join("home")),
            bridge: Some(test_bridge(directory.path())),
            ..Default::default()
        };
        assert!(matches!(
            AntigravityTransport::new(AntigravityConfig {
                effort: Some("ultra".to_string()),
                ..base.clone()
            }),
            Err(AntigravityError::Protocol(_))
        ));
        assert!(matches!(
            AntigravityTransport::with_conversation_id(base, "../other-session".to_string()),
            Err(AntigravityError::Protocol(_))
        ));
    }

    #[test]
    fn auth_errors_are_classified_without_returning_diagnostics() {
        assert!(text_indicates_auth_failure(
            "Authentication failed: OAuth token expired"
        ));
        assert!(!text_indicates_auth_failure("model backend unavailable"));
        assert!(!text_indicates_auth_failure(
            "the sign in button was not found on the page"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_cli_paths_are_rejected() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = directory.path().join("agy");
        std::fs::write(&script, "#!/bin/sh\n").expect("write fake agy");
        let config = AntigravityConfig {
            cli_path: Some(script),
            home: Some(directory.path().join("home")),
            bridge: Some(test_bridge(directory.path())),
            ..Default::default()
        };
        assert!(matches!(
            find_agy_cli(&config),
            Err(AntigravityError::CliNotFound(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transport_runs_fresh_then_resumed_turn_without_api_keys() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let dir = directory.path();
        let script = dir.join("agy");
        std::fs::write(
            &script,
            r#"#!/bin/sh
case "$*" in
  *--dangerously-skip-permissions*) exit 90 ;;
esac
if [ -n "${GEMINI_API_KEY+x}" ] || [ -n "${GOOGLE_API_KEY+x}" ]; then
  exit 91
fi
case "$CHAOS_CLAMP_MCP_SOCKET" in
  */bridge.sock) ;;
  *) exit 92 ;;
esac
if [ "$CHAOS_CLAMP_MCP_TOKEN" != "ephemeral-test-token" ]; then
  exit 92
fi
case "$*" in
  *"--conversation conversation-1"*)
    turns=2
    response=second
    ;;
  *)
    turns=1
    response=first
    ;;
esac
printf '{"event":"init","conversation_id":"conversation-1","init":{"model":"gemini-test","permission_mode":"request-review"}}\n'
printf '{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"%s"}}\n' "$response"
printf '{"event":"result","result":{"conversation_id":"conversation-1","status":"SUCCESS","response":"%s","num_turns":%s,"usage":{"input_tokens":1,"output_tokens":2,"thinking_tokens":3,"cache_read_tokens":4,"total_tokens":10}}}\n' "$response" "$turns"
"#,
        )
        .expect("write fake agy");
        let mut permissions = std::fs::metadata(&script)
            .expect("fake agy metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).expect("make fake agy executable");

        let config = AntigravityConfig {
            cli_path: Some(script),
            home: Some(dir.join("home")),
            cwd: Some(dir.to_path_buf()),
            model: "gemini-test".to_string(),
            print_timeout: Duration::from_secs(5),
            bridge: Some(test_bridge(dir)),
            ..Default::default()
        };
        let mut transport = AntigravityTransport::new(config).expect("create transport");

        let (tx, mut rx) = mpsc::channel(16);
        let fresh = transport
            .run_turn_streamed("first prompt", Some(&tx))
            .await
            .expect("fresh turn");
        let resumed = transport
            .run_turn("second prompt")
            .await
            .expect("resumed turn");
        drop(tx);

        assert_eq!(fresh.response, "first");
        assert_eq!(resumed.response, "second");
        assert_eq!(transport.conversation_id(), Some("conversation-1"));
        let AntigravityEvent::Result { result } = resumed.events.last().expect("result event")
        else {
            panic!("expected result event");
        };
        assert_eq!(result.num_turns, Some(2));

        let mut streamed = Vec::new();
        while let Some(event) = rx.recv().await {
            streamed.push(event);
        }
        assert_eq!(streamed.len(), 3, "only the streamed turn feeds the sink");
    }

    #[test]
    fn command_removes_metered_api_keys_and_never_auto_approves() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let dir = directory.path();
        let config = AntigravityConfig {
            home: Some(dir.join("home")),
            bridge: Some(test_bridge(dir)),
            sandbox: Some(AntigravitySandbox {
                program: PathBuf::from("/usr/lib/chaos/alcatraz-linux"),
                arg0: Some("alcatraz-linux".to_string()),
                args: vec!["--allow-network-for-proxy".to_string(), "--".to_string()],
            }),
            egress: Some(AntigravityEgress {
                proxy_url: "http://127.0.0.1:41234".to_string(),
                ca_bundle_path: Some(dir.join("egress-ca.pem")),
            }),
            ..Default::default()
        };
        let command = build_command(&PathBuf::from("/tmp/agy"), &config, None);
        let std_command = command.as_std();

        // The sandbox helper is what runs; `agy` is an argument to it.
        assert_eq!(
            std_command.get_program().to_string_lossy(),
            "/usr/lib/chaos/alcatraz-linux"
        );
        // A multicall helper dispatches on argv[0]; the standard library has no
        // getter for it, but its `Debug` output brackets the program path and
        // renders an overridden argv[0] as the first argument.
        assert!(
            format!("{std_command:?}")
                .contains("[\"/usr/lib/chaos/alcatraz-linux\"] \"alcatraz-linux\""),
            "helper must be launched under its multicall name: {std_command:?}"
        );
        let args = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "--sandbox"));
        assert!(!args.iter().any(|arg| arg.contains("dangerously")));
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("sandbox argument separator");
        assert_eq!(args[separator + 1], "/tmp/agy");

        let removed = std_command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(removed.iter().any(|name| name == "GEMINI_API_KEY"));
        assert!(removed.iter().any(|name| name == "GOOGLE_API_KEY"));
        // An inherited NO_PROXY would exempt hosts from the only permitted route.
        assert!(removed.iter().any(|name| name == "NO_PROXY"));
        assert!(removed.iter().any(|name| name == "no_proxy"));
        let envs = std_command
            .get_envs()
            .filter_map(|(name, value)| {
                Some((
                    name.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            envs.get(BRIDGE_TOKEN_ENV).map(String::as_str),
            Some("ephemeral-test-token")
        );
        assert_eq!(
            envs.get(BRIDGE_SOCKET_ENV).map(String::as_str),
            Some(dir.join("bridge.sock").to_string_lossy().as_ref())
        );
        assert_eq!(
            envs.get("HTTPS_PROXY").map(String::as_str),
            Some("http://127.0.0.1:41234")
        );
        assert_eq!(
            envs.get("SSL_CERT_FILE").map(String::as_str),
            Some(dir.join("egress-ca.pem").to_string_lossy().as_ref())
        );
    }

    #[test]
    fn managed_config_exposes_only_chaos_mcp_without_persisting_capability() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let dir = directory.path();
        let settings_path = dir.join(".gemini/antigravity-cli/settings.json");
        std::fs::create_dir_all(settings_path.parent().expect("settings parent"))
            .expect("create settings parent");
        std::fs::write(
            &settings_path,
            r#"{"theme":"dark","permissions":{"allow":["command(*)"]}}"#,
        )
        .expect("seed settings");

        let config = AntigravityConfig {
            home: Some(dir.to_path_buf()),
            bridge: Some(test_bridge(dir)),
            ..Default::default()
        };
        prepare_managed_home(&config).expect("prepare managed home");

        let mcp_bytes =
            std::fs::read(dir.join(".gemini/config/mcp_config.json")).expect("read MCP config");
        let mcp: Value = serde_json::from_slice(&mcp_bytes).expect("parse MCP config");
        assert_eq!(
            mcp["mcpServers"]["chaos"]["args"],
            serde_json::json!(["clamp-session-bridge"])
        );
        assert_eq!(
            mcp["mcpServers"]["chaos"]["command"],
            dir.join("chaos").to_string_lossy().as_ref()
        );
        assert!(!String::from_utf8_lossy(&mcp_bytes).contains("ephemeral-test-token"));
        assert!(!String::from_utf8_lossy(&mcp_bytes).contains("bridge.sock"));

        let settings_bytes = std::fs::read(&settings_path).expect("read settings");
        let settings: Value = serde_json::from_slice(&settings_bytes).expect("parse settings");
        assert_eq!(settings["theme"], "dark");
        assert_eq!(
            settings["permissions"]["allow"],
            serde_json::json!([CHAOS_MCP_ALLOW_RULE])
        );
        assert_eq!(
            settings["permissions"]["deny"],
            serde_json::json!(NATIVE_TOOL_DENY_RULES)
        );
        assert!(!String::from_utf8_lossy(&settings_bytes).contains("ephemeral-test-token"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [dir.join(".gemini/config/mcp_config.json"), settings_path] {
                assert_eq!(
                    std::fs::metadata(&path)
                        .expect("managed config metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn conversation_store_round_trips_only_for_matching_model_and_safe_ids() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store =
            AntigravityConversationStore::new(directory.path().join("state/conversation.json"));

        assert_eq!(store.load("gemini-3.1-pro-low"), None);
        store
            .save("gemini-3.1-pro-low", "conversation-123")
            .expect("save conversation state");
        assert_eq!(
            store.load("gemini-3.1-pro-low"),
            Some("conversation-123".to_string())
        );
        assert_eq!(store.load("gemini-3.1-pro-high"), None);

        store
            .save("gemini-3.1-pro-low", "../other-session")
            .expect("save unsafe conversation state");
        assert_eq!(store.load("gemini-3.1-pro-low"), None);

        store.clear();
        assert_eq!(store.load("gemini-3.1-pro-low"), None);
        store.clear();
    }
}
