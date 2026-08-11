//! Google Antigravity CLI subprocess transport.
//!
//! This transport deliberately leaves Google credentials in `agy`'s private
//! home directory. It invokes one non-interactive process per turn, parses the
//! documented stream-JSON output, and resumes later turns by provider
//! conversation ID.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

const DEFAULT_PRINT_TIMEOUT: Duration = Duration::from_secs(300);
const STDERR_CLASSIFICATION_LIMIT: usize = 16 * 1024;
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

/// Tool-authority level currently guaranteed by the Antigravity transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntigravityToolAuthority {
    /// `agy` runs sandboxed, native tools are denied, and its sole action
    /// surface is the session-scoped Chaos MCP bridge.
    ChaosSessionBridge,
}

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
    Spawn(#[from] std::io::Error),

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
}

impl AntigravityTransport {
    pub fn new(config: AntigravityConfig) -> Result<Self, AntigravityError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            conversation_id: None,
        })
    }

    pub fn with_conversation_id(
        config: AntigravityConfig,
        conversation_id: String,
    ) -> Result<Self, AntigravityError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            conversation_id: Some(conversation_id),
        })
    }

    pub fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn tool_authority(&self) -> AntigravityToolAuthority {
        AntigravityToolAuthority::ChaosSessionBridge
    }

    /// Run one prompt through `agy`, resuming the provider conversation when
    /// this transport has already completed a turn.
    pub async fn run_turn(&mut self, prompt: &str) -> Result<AntigravityTurn, AntigravityError> {
        let cli_path = find_agy_cli(&self.config)?;
        prepare_managed_home(&self.config)?;
        let mut command = build_command(&cli_path, &self.config, self.conversation_id.as_deref());
        command.arg("--print").arg(prompt);

        let output = tokio::time::timeout(self.config.print_timeout, command.output())
            .await
            .map_err(|_| AntigravityError::Timeout)??;

        let stderr = bounded_text(&output.stderr);
        if !output.status.success() {
            if text_indicates_auth_failure(&stderr) {
                return Err(AntigravityError::AuthenticationUnavailable);
            }
            return Err(AntigravityError::InvocationFailed(
                output.status.code().map_or_else(
                    || "terminated by signal".to_string(),
                    |code| code.to_string(),
                ),
            ));
        }

        let events = parse_events(&output.stdout)?;
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
        if path.is_file() {
            return Ok(path.clone());
        }
        return Err(AntigravityError::CliNotFound(format!(
            "specified path does not exist: {}",
            path.display()
        )));
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
    let mut command = Command::new(cli_path);
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
        Err(error) => Err(AntigravityError::Spawn(error)),
    }
}

fn atomic_write_private_json(path: &Path, value: &Value) -> Result<(), AntigravityError> {
    let parent = path.parent().ok_or_else(|| {
        AntigravityError::Protocol(format!(
            "managed Antigravity configuration has no parent: {}",
            path.display()
        ))
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
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        AntigravityError::Protocol(format!(
            "failed to encode managed Antigravity configuration: {error}"
        ))
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&temporary_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = std::fs::rename(&temporary_path, path) {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(AntigravityError::Spawn(error));
    }
    Ok(())
}

fn parse_events(stdout: &[u8]) -> Result<Vec<AntigravityEvent>, AntigravityError> {
    let stdout = std::str::from_utf8(stdout)
        .map_err(|_| AntigravityError::Protocol("stdout was not valid UTF-8".to_string()))?;
    let mut events = Vec::new();
    for (index, line) in stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event = serde_json::from_str(line).map_err(|err| {
            AntigravityError::Protocol(format!("invalid JSONL event on line {}: {err}", index + 1))
        })?;
        events.push(event);
    }
    Ok(events)
}

fn bounded_text(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(STDERR_CLASSIFICATION_LIMIT);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn text_indicates_auth_failure(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "not logged in",
        "not authenticated",
        "authentication required",
        "authentication failed",
        "oauth token",
        "sign in",
        "login required",
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

    #[test]
    fn parses_observed_stream_json() {
        let stdout = br#"{"event":"init","conversation_id":"conversation-1","init":{"model":"gemini-3.1-pro-low","permission_mode":"request-review","tools":["run_command"]}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"hello\n","usage":{"input_tokens":11,"output_tokens":7,"thinking_tokens":5,"cache_read_tokens":3,"total_tokens":26}}}
{"event":"result","result":{"conversation_id":"conversation-1","status":"SUCCESS","response":"hello\n","duration_seconds":1.5,"num_turns":1,"usage":{"input_tokens":11,"output_tokens":7,"thinking_tokens":5,"cache_read_tokens":3,"total_tokens":26}}}
"#;
        let events = parse_events(stdout).expect("events should parse");
        assert_eq!(events.len(), 3);
        let AntigravityEvent::Result { result } = &events[2] else {
            panic!("expected result event");
        };
        assert_eq!(result.conversation_id, "conversation-1");
        assert_eq!(result.response, "hello\n");
        assert_eq!(result.usage.as_ref().expect("usage").thinking_tokens, 5);
    }

    #[test]
    fn parses_tool_steps_and_unknown_step_types() {
        let stdout = br#"{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":4,"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"parameters":{"ServerName":"chaos","ToolName":"read_file"},"output":"ok"}}}
{"event":"step_update","step_update":{"conversation_id":"conversation-1","step_index":5,"state":"DONE","step_type":"future_checkpoint_kind"}}
"#;
        let events = parse_events(stdout).expect("events should parse");
        let AntigravityEvent::StepUpdate { step_update } = &events[0] else {
            panic!("expected tool step");
        };
        assert_eq!(step_update.tool_name.as_deref(), Some("call_mcp_tool"));
        assert_eq!(
            step_update.tool_info.as_ref().expect("tool info")["output"],
            "ok"
        );
        let AntigravityEvent::StepUpdate { step_update } = &events[1] else {
            panic!("expected unknown step");
        };
        assert_eq!(step_update.step_type, "future_checkpoint_kind");
    }

    #[test]
    fn rejects_unknown_effort() {
        let config = AntigravityConfig {
            effort: Some("ultra".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            AntigravityTransport::new(config),
            Err(AntigravityError::Protocol(_))
        ));
    }

    #[test]
    fn auth_errors_are_classified_without_returning_diagnostics() {
        assert!(text_indicates_auth_failure(
            "Authentication failed: OAuth token expired"
        ));
        assert!(!text_indicates_auth_failure("model backend unavailable"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transport_runs_fresh_then_resumed_turn_without_api_keys() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::SystemTime;

        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chaos-antigravity-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create test directory");
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
            cwd: Some(dir.clone()),
            model: "gemini-test".to_string(),
            print_timeout: Duration::from_secs(5),
            bridge: Some(test_bridge(&dir)),
            ..Default::default()
        };
        let mut transport = AntigravityTransport::new(config).expect("create transport");

        let fresh = transport
            .run_turn("first prompt")
            .await
            .expect("fresh turn");
        let resumed = transport
            .run_turn("second prompt")
            .await
            .expect("resumed turn");

        assert_eq!(fresh.response, "first");
        assert_eq!(resumed.response, "second");
        assert_eq!(transport.conversation_id(), Some("conversation-1"));
        let AntigravityEvent::Result { result } = resumed.events.last().expect("result event")
        else {
            panic!("expected result event");
        };
        assert_eq!(result.num_turns, Some(2));

        std::fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn command_removes_metered_api_keys_and_never_auto_approves() {
        let dir = std::env::temp_dir();
        let config = AntigravityConfig {
            home: Some(dir.join("chaos-antigravity-command-test")),
            bridge: Some(test_bridge(&dir)),
            ..Default::default()
        };
        let command = build_command(&PathBuf::from("/tmp/agy"), &config, None);
        let std_command = command.as_std();
        let args = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "--sandbox"));
        assert!(!args.iter().any(|arg| arg.contains("dangerously")));

        let removed = std_command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(removed.iter().any(|name| name == "GEMINI_API_KEY"));
        assert!(removed.iter().any(|name| name == "GOOGLE_API_KEY"));
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
    }

    #[test]
    fn managed_config_exposes_only_chaos_mcp_without_persisting_capability() {
        use std::time::SystemTime;

        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chaos-antigravity-config-test-{}-{unique}",
            std::process::id()
        ));
        let settings_path = dir.join(".gemini/antigravity-cli/settings.json");
        std::fs::create_dir_all(settings_path.parent().expect("settings parent"))
            .expect("create settings parent");
        std::fs::write(
            &settings_path,
            r#"{"theme":"dark","permissions":{"allow":["command(*)"]}}"#,
        )
        .expect("seed settings");

        let config = AntigravityConfig {
            home: Some(dir.clone()),
            bridge: Some(test_bridge(&dir)),
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
            assert_eq!(
                std::fs::metadata(dir.join(".gemini/config/mcp_config.json"))
                    .expect("MCP metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&settings_path)
                    .expect("settings metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        std::fs::remove_dir_all(dir).expect("remove test directory");
    }
}
