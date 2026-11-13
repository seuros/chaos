//! Google Antigravity CLI subprocess transport.
//!
//! This transport deliberately leaves Google credentials in `agy`'s private
//! home directory. It invokes one non-interactive process per turn, parses the
//! documented stream-JSON output as it arrives, and resumes later turns by
//! provider conversation ID.

use chaos_snitch::tracing;
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
    /// Daemon-only environment names that must never reach the native CLI.
    pub env_remove: Vec<String>,
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
    /// Environment supplied by the platform sandbox backend.
    pub env: std::collections::HashMap<String, String>,
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
            env_remove: Vec::new(),
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
    /// Fields this adapter does not model. agy reports why a run ended with
    /// a non-`SUCCESS` status in fields of its result event, so they are kept
    /// to explain the failure instead of being dropped by deserialisation.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
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
    child: Option<crate::child::NativeChild>,
}

impl AntigravityTransport {
    pub fn new(config: AntigravityConfig) -> Result<Self, AntigravityError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            conversation_id: None,
            managed_home_prepared: false,
            child: None,
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
            child: None,
        })
    }

    pub fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Reap an invocation even when its driving future was cancelled.
    pub async fn terminate(&mut self) -> Result<(), AntigravityError> {
        if let Some(child) = self.child.as_mut() {
            child.terminate().await?;
            self.child = None;
        }
        Ok(())
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
        self.terminate().await?;
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

        // Logs that exist before this child starts cannot be its own; the
        // snapshot is how a failed run's diagnostic is attributed to it.
        let logs_before = self.config.home.as_deref().map(agy_log_files);
        let child = self
            .child
            .insert(crate::child::NativeChild::spawn(&mut command)?)
            .child();
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
                tokio::join!(write_input, drive_invocation(child, sink));
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
                self.terminate().await?;
                return Err(AntigravityError::Timeout);
            }
        };
        // A reaped parent is not proof that its descendants stopped.
        self.terminate().await?;

        if !invocation.status.success() {
            // Classification is driven by this child's own stderr only.
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
            if !tail.is_empty() {
                return Err(AntigravityError::InvocationFailed(format!(
                    "{status}: {tail}"
                )));
            }
            // In print mode agy writes its diagnostics to a log file under the
            // managed home, not to stderr, so a silent tail used to surface as a
            // bare "status 1" even when agy had said why (e.g. an unknown model
            // and the list of available ones). The log cannot be tied to this
            // child with certainty, so it is offered as a labelled candidate,
            // never fed to classification.
            let candidate = self
                .config
                .home
                .as_deref()
                .zip(logs_before.as_ref())
                .and_then(|(home, before)| candidate_agy_log_diagnostic(home, before));
            return Err(AntigravityError::InvocationFailed(match candidate {
                Some(candidate) => {
                    format!("{status}; candidate recent agy log diagnostic: {candidate}")
                }
                None => status,
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
            return Err(AntigravityError::InvocationFailed(failed_result_detail(
                result,
                &events,
                &invocation.stderr_tail,
            )));
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
            command.envs(&sandbox.env);
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
    for name in &config.env_remove {
        command.env_remove(name);
    }
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

/// Prefix glog adds when agy logs before its logger is initialised.
const AGY_EARLY_LOG_PREFIX: &str = "ERROR: logging before google.Init: ";
/// Upper bound on one value lifted from a non-`SUCCESS` result.
const FAILED_RESULT_VALUE_LIMIT: usize = 256;
/// Upper bound on the stderr tail carried by a non-`SUCCESS` result.
const FAILED_RESULT_STDERR_LIMIT: usize = 1024;
/// Upper bound on the whole diagnostic for a non-`SUCCESS` result.
const FAILED_RESULT_DETAIL_LIMIT: usize = 2 * 1024;
/// Result-event keys read as a diagnosis, compared lowercased without `_`.
/// Everything else in the event is ignored: an unknown field may carry the
/// prompt, a request body or a credential.
const FAILED_RESULT_ERROR_KEYS: &[&str] = &[
    "error",
    "errormessage",
    "errorcode",
    "errortype",
    "errorreason",
    "errorstatus",
    "errordetail",
    "reason",
    "statuscode",
    "code",
];
/// Keys read inside an object-valued error key; nothing deeper is read.
const FAILED_RESULT_NESTED_KEYS: &[&str] = &["code", "status", "message", "reason", "type"];

/// Explains a result event whose status is not `SUCCESS`.
///
/// agy exits 0 in that case, so the exit status says nothing and its log
/// usually holds only start-up noise. What it does say is in the result
/// event's error fields, the step it stopped on, and its stderr. Only an
/// allow-listed projection of the event is used, each value is sanitised
/// and bounded, and so is the whole: the response text, step text and any
/// other field are left out, since they can be the model's or the user's
/// words rather than a diagnosis.
fn failed_result_detail(
    result: &AntigravityResult,
    events: &[AntigravityEvent],
    stderr_tail: &str,
) -> String {
    let mut detail = diagnostic_token(&result.status);
    let fields = failed_result_error_fields(&result.extra);
    if !fields.is_empty() {
        detail.push_str("; result detail: ");
        detail.push_str(&fields.join(", "));
    }
    let last_step = events.iter().rev().find_map(|event| match event {
        AntigravityEvent::StepUpdate { step_update } => Some(step_update),
        _ => None,
    });
    if let Some(step) = last_step {
        detail.push_str(&format!(
            "; last step {}: {} {}",
            step.step_index,
            diagnostic_token(&step.step_type),
            diagnostic_token(&step.state)
        ));
        if let Some(tool) = step.tool_name.as_deref() {
            detail.push_str(&format!(" ({})", diagnostic_token(tool)));
        }
    }
    let stderr = stderr_tail.trim();
    if !stderr.is_empty() {
        detail.push_str("; stderr: ");
        // Sanitise before clipping so a cut cannot strip the context
        // ("Bearer ", "password=") that marks the next word as a secret.
        let sanitised = sanitise_diagnostic(bounded_tail(stderr, SANITISE_WINDOW));
        detail.push_str(bounded_tail(&sanitised, FAILED_RESULT_STDERR_LIMIT));
    }
    bounded_head(&detail, FAILED_RESULT_DETAIL_LIMIT).to_string()
}

/// `key=value` pairs for the allow-listed error keys of a result event.
fn failed_result_error_fields(extra: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut fields = Vec::new();
    for (key, value) in extra {
        let normalised = key.to_ascii_lowercase().replace('_', "");
        if !FAILED_RESULT_ERROR_KEYS.contains(&normalised.as_str()) {
            continue;
        }
        let key = diagnostic_token(key);
        match value {
            Value::Object(nested) => {
                for (inner, value) in nested {
                    if FAILED_RESULT_NESTED_KEYS.contains(&inner.to_ascii_lowercase().as_str())
                        && let Some(value) = diagnostic_scalar(value)
                    {
                        fields.push(format!("{key}.{}={value}", diagnostic_token(inner)));
                    }
                }
            }
            value => {
                if let Some(value) = diagnostic_scalar(value) {
                    fields.push(format!("{key}={value}"));
                }
            }
        }
    }
    fields
}

/// A string, number or boolean as sanitised, bounded text; nothing else.
fn diagnostic_scalar(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => return None,
    };
    let sanitised = sanitise_diagnostic(bounded_head(&text, SANITISE_WINDOW));
    Some(bounded_head(&sanitised, FAILED_RESULT_VALUE_LIMIT).to_string())
}

/// An identifier-like field (status, step type, key) kept to a safe alphabet.
fn diagnostic_token(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        .take(64)
        .collect()
}

/// How much text is sanitised before the final clip. Far larger than any
/// clip, so the window's own cut lands in text the clip discards.
const SANITISE_WINDOW: usize = 16 * 1024;

/// Secret-bearing key names: a key containing one of these marks its value.
const SECRET_KEY: &str = r"[A-Za-z0-9_.-]*(?i:token|secret|passw(?:or)?d|api[_-]?key|credential|cookie|session|authorization|auth|private[_-]?key|signature)[A-Za-z0-9_.-]*";

/// How many contextual redaction rules there are; a test checks all compiled.
#[cfg(test)]
const REDACTION_RULE_COUNT: usize = 6;

static CONTEXTUAL_REDACTIONS: std::sync::LazyLock<Vec<(regex_lite::Regex, &'static str)>> =
    std::sync::LazyLock::new(|| {
        let rules = [
            // Bearer / Basic credentials, wherever they appear.
            (
                r#"(?i)\b(bearer|basic)\s+[^\s"',;]+"#.to_string(),
                "$1 [redacted]",
            ),
            // A secret key with a double-quoted value, escapes and spaces included.
            (
                format!(r#"("?{SECRET_KEY}"?\s*[:=]\s*)"(?:[^"\\]|\\.)*""#),
                "$1\"[redacted]\"",
            ),
            // The same with a single-quoted value.
            (
                format!(r"('?{SECRET_KEY}'?\s*[:=]\s*)'(?:[^'\\]|\\.)*'"),
                "$1'[redacted]'",
            ),
            // An unquoted value: everything to the next separator or line end,
            // so a value with spaces goes whole.
            (
                format!(r#"(\b{SECRET_KEY}\s*[:=]\s*)[^\s"'\[][^;,&\r\n]*"#),
                "$1[redacted]",
            ),
            // Known key shapes too short for the opaque-run rule.
            (
                r"\b(?:sk-|AIza|ya29\.|gh[pousr]_|xox[abpr]-)[A-Za-z0-9_.-]+".to_string(),
                "[redacted]",
            ),
            // URL query strings carry signatures and keys.
            (r#"\?[^\s"']+"#.to_string(), "?[redacted]"),
        ];
        // A rule that fails to compile is dropped here, so a test pins
        // that all of them did (`REDACTION_RULE_COUNT`).
        rules
            .into_iter()
            .filter_map(|(pattern, replacement)| {
                regex_lite::Regex::new(&pattern)
                    .ok()
                    .map(|regex| (regex, replacement))
            })
            .collect()
    });

/// Redacts credentials from free text before it leaves the adapter: bearer
/// and basic credentials, values of secret-named keys (whole, quoted or
/// not), known key shapes, URL query strings, then any long opaque run.
/// Control characters collapse to spaces.
pub(crate) fn sanitise_diagnostic(text: &str) -> String {
    let mut text = text.to_string();
    for (pattern, replacement) in CONTEXTUAL_REDACTIONS.iter() {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    redact_opaque_runs(&text)
}

/// Collapses control characters and redacts long opaque runs (keys, tokens,
/// signatures).
fn redact_opaque_runs(text: &str) -> String {
    const OPAQUE_RUN: usize = 32;
    let is_opaque =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=' | '.');
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= OPAQUE_RUN {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if is_opaque(c) {
            run.push(c);
            continue;
        }
        flush(&mut run, &mut out);
        out.push(if c.is_control() { ' ' } else { c });
    }
    flush(&mut run, &mut out);
    out
}

/// The first `limit` bytes of `text`, cut back to a character boundary.
fn bounded_head(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The last `limit` bytes of `text`, moved forward to a character boundary.
fn bounded_tail(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}
/// Upper bound on the diagnostic text lifted from an agy log.
const AGY_LOG_ERROR_LIMIT: usize = 2 * 1024;
/// Upper bound on how much of an agy log is read from disk.
const AGY_LOG_READ_LIMIT: u64 = 64 * 1024;

/// The regular `cli-*.log` files currently in the managed agy log directory.
fn agy_log_files(home: &Path) -> std::collections::BTreeSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home.join(".gemini/antigravity-cli/log")) else {
        return std::collections::BTreeSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("cli-")
                && name.ends_with(".log")
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .map(|entry| entry.path())
        .collect()
}

/// Returns the last error group from the single agy log created since
/// `before` was taken, or `None` when there is no new log or more than one.
/// This narrows the choice but does not prove ownership: if this child wrote
/// no log while an overlapping invocation in the same home wrote exactly one,
/// that one is returned. Callers must present the result as a candidate and
/// must not classify on it.
fn candidate_agy_log_diagnostic(
    home: &Path,
    before: &std::collections::BTreeSet<PathBuf>,
) -> Option<String> {
    let mut created = agy_log_files(home)
        .into_iter()
        .filter(|path| !before.contains(path));
    let log = created.next()?;
    if created.next().is_some() {
        return None;
    }
    agy_log_error_lines(&read_bounded_tail(&log, AGY_LOG_READ_LIMIT)?)
}

/// Reads at most the last `limit` bytes of a regular file without loading the
/// rest, and never more than `limit` even if the file grows meanwhile.
fn read_bounded_tail(path: &Path, limit: u64) -> Option<String> {
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;

    let mut file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    file.seek(SeekFrom::Start(metadata.len().saturating_sub(limit)))
        .ok()?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether `line` opens a glog record: severity, MMDD, a space and hh:mm:ss
/// ("E1013 19:41:07.548929 1 file.go:265] ..."). Checking the clock too keeps
/// a continuation line such as "E12345: ..." from being read as a header.
fn is_glog_header(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 14
        && matches!(bytes[0], b'I' | b'W' | b'E' | b'F')
        && bytes[1..5].iter().all(u8::is_ascii_digit)
        && bytes[5] == b' '
        && bytes[6..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b':'
        && bytes[9..11].iter().all(u8::is_ascii_digit)
        && bytes[11] == b':'
        && bytes[12..14].iter().all(u8::is_ascii_digit)
}

fn agy_log_error_lines(text: &str) -> Option<String> {
    // Each error-level line opens a group that also takes its unprefixed
    // continuation lines. Only the last group is returned: agy logs harmless
    // errors early on (e.g. "admin controls not applicable") and exits on the
    // last one.
    let mut last_group: Option<Vec<String>> = None;
    let mut in_error = false;
    for line in text.lines() {
        let body = line.strip_prefix(AGY_EARLY_LOG_PREFIX).unwrap_or(line);
        if is_glog_header(body) {
            in_error = matches!(body.as_bytes()[0], b'E' | b'F');
            if in_error {
                // Drop the glog header ("E1013 19:41:07.548929 1 file.go:265]").
                let message = body.split_once("] ").map_or(body, |(_, rest)| rest);
                last_group = Some(vec![message.trim().to_string()]);
            }
        } else if in_error
            && !line.trim().is_empty()
            && let Some(group) = last_group.as_mut()
        {
            group.push(line.trim().to_string());
        }
    }
    let joined = last_group?.join("\n");
    if joined.len() <= AGY_LOG_ERROR_LIMIT {
        return Some(joined);
    }
    let mut end = AGY_LOG_ERROR_LIMIT;
    while !joined.is_char_boundary(end) {
        end -= 1;
    }
    Some(joined[..end].to_string())
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
