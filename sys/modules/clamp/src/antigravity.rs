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
use tokio::io::AsyncWriteExt;
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
/// `alcatraz`) and `args` are its policy arguments, terminated by `--`.
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
        // Stream input activates non-interactive mode by itself. A bare
        // `--print` is an optional-value flag in agy 1.1.22 and consumes the
        // following `--input-format` as its prompt.
        command.args(["--input-format", "stream-json"]);

        let mut child = command.spawn().map_err(AntigravityError::Spawn)?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            AntigravityError::Protocol("Antigravity stdin was not captured".to_string())
        })?;
        let mut input = serde_json::to_vec(&serde_json::json!({
            "event": "user",
            "message": {
                "content": prompt
            }
        }))
        .map_err(|error| {
            AntigravityError::Protocol(format!(
                "failed to encode Antigravity stream input: {error}"
            ))
        })?;
        input.push(b'\n');
        let deadline = self.config.print_timeout + PRINT_TIMEOUT_GRACE;
        let invocation = match tokio::time::timeout(deadline, async {
            let write_input = async {
                let result = stdin.write_all(&input).await;
                drop(stdin);
                result
            };
            let (input_result, invocation_result) =
                tokio::join!(write_input, drive_invocation(&mut child, sink));
            let invocation = invocation_result?;
            if invocation.status.success() {
                input_result.map_err(AntigravityError::Spawn)?;
            }
            Ok::<_, AntigravityError>(invocation)
        })
        .await
        {
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
            // The tail is carried into the error: an exit code alone says
            // nothing about which of the CLI, the sandbox helper, or the
            // managed configuration rejected the invocation.
            let status = invocation.status.code().map_or_else(
                || "terminated by signal".to_string(),
                |code| code.to_string(),
            );
            let tail = invocation.stderr_tail.trim();
            return Err(AntigravityError::InvocationFailed(if tail.is_empty() {
                status
            } else {
                format!("{status}: {tail}")
            }));
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
    command.stdin(std::process::Stdio::piped());
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
            // Antigravity creates zero-byte placeholder configuration files
            // during first-run initialization. This home is dedicated to the
            // managed clamp, so an empty placeholder has the same meaning as a
            // missing file and can be initialized safely.
            if bytes.iter().all(u8::is_ascii_whitespace) {
                return Ok(Value::Object(Default::default()));
            }
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
mod tests;
