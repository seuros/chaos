use crate::exec::ExecToolCallOutput;
use crate::network_policy_decision::NetworkPolicyDecisionPayload;
use crate::truncate::TruncationPolicy;
use crate::truncate::truncate_text;
use chaos_epoll::CancelErr;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::RateLimitSnapshot;
use http::StatusCode;
use jiff::Timestamp;
use serde_json;
use std::io;
use std::time::Duration;
use thiserror::Error;
use tokio::task::JoinError;

pub type Result<T> = std::result::Result<T, ChaosErr>;

/// Limit UI error messages to a reasonable size while keeping useful context.
const ERROR_MESSAGE_UI_MAX_BYTES: usize = 2 * 1024; // 2 KiB

#[derive(Error, Debug)]
pub enum SandboxErr {
    /// Error from sandbox execution
    #[error(
        "sandbox denied exec error, exit code: {}, stdout: {}, stderr: {}",
        .output.exit_code, .output.stdout.text, .output.stderr.text
    )]
    Denied {
        output: Box<ExecToolCallOutput>,
        network_policy_decision: Option<NetworkPolicyDecisionPayload>,
    },

    /// Error from linux seccomp filter setup
    #[cfg(target_os = "linux")]
    #[error("seccomp setup error")]
    SeccompInstall(#[from] seccompiler::Error),

    /// Error from linux seccomp backend
    #[cfg(target_os = "linux")]
    #[error("seccomp backend error")]
    SeccompBackend(#[from] seccompiler::BackendError),

    /// Command timed out
    #[error("command timed out")]
    Timeout { output: Box<ExecToolCallOutput> },

    /// Command was killed by a signal
    #[error("command was killed by a signal")]
    Signal(i32),

    /// Error from linux landlock
    #[error("Landlock was not able to fully enforce all sandbox rules")]
    LandlockRestrict,
}

#[derive(Error, Debug)]
pub enum ChaosErr {
    #[error("turn aborted. Tell the model what to do differently and try again.")]
    TurnAborted,

    /// Returned by ResponsesClient when the SSE stream disconnects or errors out **after** the HTTP
    /// handshake has succeeded but **before** it finished emitting `response.completed`.
    ///
    /// The Session loop treats this as a transient error and will automatically retry the turn.
    ///
    /// Optionally includes the requested delay before retrying the turn.
    #[error("stream disconnected before completion: {0}")]
    Stream(String, Option<Duration>),

    #[error(
        "Chaos ran out of room in the model's context window. Start a new thread or clear earlier history before retrying."
    )]
    ContextWindowExceeded,

    #[error("no thread with id: {0}")]
    ProcessNotFound(ProcessId),

    #[error("agent thread limit reached (max {max_threads})")]
    AgentLimitReached { max_threads: usize },

    #[error("session configured event was not the first event in the stream")]
    SessionConfiguredNotFirstEvent,

    /// Returned by run_command_stream when the spawned child process timed out (10s).
    #[error("timeout waiting for child process to exit")]
    Timeout,

    /// Returned by run_command_stream when the child could not be spawned (its stdout/stderr pipes
    /// could not be captured). Analogous to the previous `ChaosError::Spawn` variant.
    #[error("spawn failed: child stdout/stderr not captured")]
    Spawn,

    /// Returned by run_command_stream when the user pressed Ctrl‑C (SIGINT). Session uses this to
    /// surface a polite FunctionCallOutput back to the model instead of crashing the CLI.
    #[error("interrupted (Ctrl-C). Tell the model what to do differently and try again.")]
    Interrupted,

    /// Unexpected HTTP status code.
    #[error("{0}")]
    UnexpectedStatus(UnexpectedResponseError),

    /// Invalid request.
    #[error("{0}")]
    InvalidRequest(String),

    /// Invalid image.
    #[error("Image poisoning")]
    InvalidImageRequest(),

    #[error("{0}")]
    UsageLimitReached(UsageLimitReachedError),

    #[error("Selected model is at capacity. Please try a different model.")]
    ServerOverloaded,

    #[error("{0}")]
    ResponseStreamFailed(ResponseStreamFailed),

    #[error("{0}")]
    ConnectionFailed(ConnectionFailedError),

    #[error("Quota exceeded. The vendor refuses to serve more requests on this account.")]
    QuotaExceeded,

    #[error("We're currently experiencing high demand, which may cause temporary errors.")]
    InternalServerError,

    /// Retry limit exceeded.
    #[error("{0}")]
    RetryLimit(RetryLimitReachedError),

    /// Agent loop died unexpectedly
    #[error("internal error; agent loop died unexpectedly")]
    InternalAgentDied,

    /// Sandbox error
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxErr),

    #[error("alcatraz-linux was required but not provided")]
    LandlockSandboxExecutableNotProvided,

    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),

    #[error("{0}")]
    RefreshTokenFailed(RefreshTokenFailedError),

    #[error("Fatal error: {0}")]
    Fatal(String),

    // -----------------------------------------------------------------
    // Automatic conversions for common external error types
    // -----------------------------------------------------------------
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    LandlockRuleset(#[from] landlock::RulesetError),

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    LandlockPathFd(#[from] landlock::PathFdError),

    #[error(transparent)]
    TokioJoin(#[from] JoinError),

    #[error("{0}")]
    EnvVar(EnvVarError),

    /// Raised by the auth preflight when the active provider requires
    /// credentials (cached login, env key, or bearer) and none were found.
    /// Non-retryable: the outer loop must stop and the client is expected to
    /// surface a login prompt rather than firing a doomed request.
    #[error("{0}")]
    ProviderAuthMissing(ProviderAuthMissingError),
}

impl From<CancelErr> for ChaosErr {
    fn from(_: CancelErr) -> Self {
        ChaosErr::TurnAborted
    }
}

impl ChaosErr {
    pub fn is_retryable(&self) -> bool {
        match self {
            ChaosErr::TurnAborted
            | ChaosErr::Interrupted
            | ChaosErr::EnvVar(_)
            | ChaosErr::ProviderAuthMissing(_)
            | ChaosErr::Fatal(_)
            | ChaosErr::QuotaExceeded
            | ChaosErr::InvalidImageRequest()
            | ChaosErr::InvalidRequest(_)
            | ChaosErr::RefreshTokenFailed(_)
            | ChaosErr::UnsupportedOperation(_)
            | ChaosErr::Sandbox(_)
            | ChaosErr::LandlockSandboxExecutableNotProvided
            | ChaosErr::RetryLimit(_)
            | ChaosErr::ContextWindowExceeded
            | ChaosErr::ProcessNotFound(_)
            | ChaosErr::AgentLimitReached { .. }
            | ChaosErr::Spawn
            | ChaosErr::SessionConfiguredNotFirstEvent
            | ChaosErr::UsageLimitReached(_)
            | ChaosErr::ServerOverloaded => false,
            ChaosErr::Stream(..)
            | ChaosErr::Timeout
            | ChaosErr::UnexpectedStatus(_)
            | ChaosErr::ResponseStreamFailed(_)
            | ChaosErr::ConnectionFailed(_)
            | ChaosErr::InternalServerError
            | ChaosErr::InternalAgentDied
            | ChaosErr::Io(_)
            | ChaosErr::Json(_)
            | ChaosErr::TokioJoin(_) => true,
            #[cfg(target_os = "linux")]
            ChaosErr::LandlockRuleset(_) | ChaosErr::LandlockPathFd(_) => false,
        }
    }
}

#[derive(Debug)]
pub struct ConnectionFailedError {
    pub source: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for ConnectionFailedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Connection failed: {}", self.source)
    }
}

#[derive(Debug)]
pub struct ResponseStreamFailed {
    pub source: Box<dyn std::error::Error + Send + Sync>,
    pub request_id: Option<String>,
}

impl std::fmt::Display for ResponseStreamFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Error while reading the server response: {}{}",
            self.source,
            self.request_id
                .as_ref()
                .map(|id| format!(", request id: {id}"))
                .unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct RefreshTokenFailedError {
    pub reason: RefreshTokenFailedReason,
    pub message: String,
}

impl RefreshTokenFailedError {
    pub fn new(reason: RefreshTokenFailedReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTokenFailedReason {
    Expired,
    Exhausted,
    Revoked,
    Other,
}

#[derive(Debug)]
pub struct UnexpectedResponseError {
    pub status: StatusCode,
    pub body: String,
    pub url: Option<String>,
    pub cf_ray: Option<String>,
    pub request_id: Option<String>,
    pub identity_authorization_error: Option<String>,
    pub identity_error_code: Option<String>,
}

const CLOUDFLARE_BLOCKED_MESSAGE: &str =
    "Access blocked by Cloudflare. This usually happens when connecting from a restricted region";
const UNEXPECTED_RESPONSE_BODY_MAX_BYTES: usize = 1000;

impl UnexpectedResponseError {
    fn display_body(&self) -> String {
        if let Some(message) = self.extract_error_message() {
            return message;
        }

        let trimmed_body = self.body.trim();
        if trimmed_body.is_empty() {
            return "Unknown error".to_string();
        }

        truncate_with_ellipsis(trimmed_body, UNEXPECTED_RESPONSE_BODY_MAX_BYTES)
    }

    fn extract_error_message(&self) -> Option<String> {
        let json = serde_json::from_str::<serde_json::Value>(&self.body).ok()?;
        let message = json
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str)?;
        let message = message.trim();
        if message.is_empty() {
            None
        } else {
            Some(message.to_string())
        }
    }

    fn friendly_message(&self) -> Option<String> {
        if self.status != StatusCode::FORBIDDEN {
            return None;
        }

        if !self.body.contains("Cloudflare") || !self.body.contains("blocked") {
            return None;
        }

        let status = self.status;
        let mut message = format!("{CLOUDFLARE_BLOCKED_MESSAGE} (status {status})");
        if let Some(url) = &self.url {
            message.push_str(&format!(", url: {url}"));
        }
        if let Some(cf_ray) = &self.cf_ray {
            message.push_str(&format!(", cf-ray: {cf_ray}"));
        }
        if let Some(id) = &self.request_id {
            message.push_str(&format!(", request id: {id}"));
        }
        if let Some(auth_error) = &self.identity_authorization_error {
            message.push_str(&format!(", auth error: {auth_error}"));
        }
        if let Some(error_code) = &self.identity_error_code {
            message.push_str(&format!(", auth error code: {error_code}"));
        }

        Some(message)
    }
}

impl std::fmt::Display for UnexpectedResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(friendly) = self.friendly_message() {
            write!(f, "{friendly}")
        } else {
            let status = self.status;
            let body = self.display_body();
            let mut message = format!("unexpected status {status}: {body}");
            if let Some(url) = &self.url {
                message.push_str(&format!(", url: {url}"));
            }
            if let Some(cf_ray) = &self.cf_ray {
                message.push_str(&format!(", cf-ray: {cf_ray}"));
            }
            if let Some(id) = &self.request_id {
                message.push_str(&format!(", request id: {id}"));
            }
            if let Some(auth_error) = &self.identity_authorization_error {
                message.push_str(&format!(", auth error: {auth_error}"));
            }
            if let Some(error_code) = &self.identity_error_code {
                message.push_str(&format!(", auth error code: {error_code}"));
            }
            write!(f, "{message}")
        }
    }
}

impl std::error::Error for UnexpectedResponseError {}

fn truncate_with_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let mut cut = max_bytes;
    while !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let mut truncated = text[..cut].to_string();
    truncated.push_str("...");
    truncated
}

#[derive(Debug)]
pub struct RetryLimitReachedError {
    pub status: StatusCode,
    pub request_id: Option<String>,
}

impl std::fmt::Display for RetryLimitReachedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exceeded retry limit, last status: {}{}",
            self.status,
            self.request_id
                .as_ref()
                .map(|id| format!(", request id: {id}"))
                .unwrap_or_default()
        )
    }
}

#[derive(Debug)]
pub struct UsageLimitReachedError {
    pub(crate) resets_at: Option<Timestamp>,
    pub(crate) rate_limits: Option<Box<RateLimitSnapshot>>,
}

impl std::fmt::Display for UsageLimitReachedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let limit_name = self
            .rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.limit_name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("chaos"));

        let refill = match self.resets_at.as_ref() {
            Some(ts) => format!("Next refill: {}.", format_retry_timestamp(ts)),
            None => "Refill ETA unknown.".to_string(),
        };

        match limit_name {
            Some(name) => write!(f, "Hallucination overdose on {name}. {refill}"),
            None => write!(f, "Hallucination overdose. {refill}"),
        }
    }
}

fn format_retry_timestamp(resets_at: &Timestamp) -> String {
    let local_reset = resets_at.to_zoned(jiff::tz::TimeZone::system());
    let local_now = now_for_retry().to_zoned(jiff::tz::TimeZone::system());
    if local_reset.date() == local_now.date() {
        local_reset.strftime("%-I:%M %p").to_string()
    } else {
        let suffix = day_suffix(local_reset.day() as u32);
        local_reset
            .strftime(&format!("%b %-d{suffix}, %Y %-I:%M %p"))
            .to_string()
    }
}

fn day_suffix(day: u32) -> &'static str {
    match day {
        11..=13 => "th",
        _ => match day % 10 {
            1 => "st",
            2 => "nd", // codespell:ignore
            3 => "rd",
            _ => "th",
        },
    }
}

#[cfg(test)]
thread_local! {
    static NOW_OVERRIDE: std::cell::RefCell<Option<Timestamp>> =
        const { std::cell::RefCell::new(None) };
}

fn now_for_retry() -> Timestamp {
    #[cfg(test)]
    {
        if let Some(now) = NOW_OVERRIDE.with(|cell| *cell.borrow()) {
            return now;
        }
    }
    Timestamp::now()
}

#[derive(Debug, Clone)]
pub struct ProviderAuthMissingError {
    /// Stable id of the provider (e.g. `openai`, `anthropic`, `xai`).
    pub provider_id: String,
    /// User-facing provider name.
    pub provider_name: String,
    /// Environment variable the provider expects, if any.
    pub env_key: Option<String>,
    /// Provider-supplied instructions for obtaining/setting the env key.
    pub env_key_instructions: Option<String>,
    /// Whether the provider also accepts an OAuth login flow (OpenAI ChatGPT).
    pub supports_oauth: bool,
}

impl std::fmt::Display for ProviderAuthMissingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "No credentials found for provider `{}`.",
            self.provider_name
        )?;
        match (&self.env_key, self.supports_oauth) {
            (Some(env_key), true) => write!(
                f,
                " Run `chaos login` or set `{env_key}` in your environment."
            )?,
            (Some(env_key), false) => {
                write!(f, " Set `{env_key}` in your environment.")?;
            }
            (None, true) => write!(f, " Run `chaos login` to sign in.")?,
            (None, false) => {
                write!(f, " Configure credentials for this provider.")?;
            }
        }
        if let Some(instructions) = &self.env_key_instructions {
            write!(f, " {instructions}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct EnvVarError {
    /// Name of the environment variable that is missing.
    pub var: String,

    /// Optional instructions to help the user get a valid value for the
    /// variable and set it.
    pub instructions: Option<String>,
}

impl std::fmt::Display for EnvVarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Missing environment variable: `{}`.", self.var)?;
        if let Some(instructions) = &self.instructions {
            write!(f, " {instructions}")?;
        }
        Ok(())
    }
}

impl ChaosErr {
    /// Minimal shim so that existing `e.downcast_ref::<ChaosErr>()` checks continue to compile
    /// after replacing `anyhow::Error` in the return signature. This mirrors the behavior of
    /// `anyhow::Error::downcast_ref` but works directly on our concrete enum.
    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        (self as &dyn std::any::Any).downcast_ref::<T>()
    }

    /// Translate core error to client-facing protocol error.
    pub fn to_chaos_ipc_error(&self) -> ChaosErrorInfo {
        match self {
            ChaosErr::ContextWindowExceeded => ChaosErrorInfo::ContextWindowExceeded,
            ChaosErr::UsageLimitReached(_) | ChaosErr::QuotaExceeded => {
                ChaosErrorInfo::UsageLimitExceeded
            }
            ChaosErr::ServerOverloaded => ChaosErrorInfo::ServerOverloaded,
            ChaosErr::RetryLimit(_) => ChaosErrorInfo::ResponseTooManyFailedAttempts {
                http_status_code: self.http_status_code_value(),
            },
            ChaosErr::ConnectionFailed(_) => ChaosErrorInfo::HttpConnectionFailed {
                http_status_code: self.http_status_code_value(),
            },
            ChaosErr::ResponseStreamFailed(_) => ChaosErrorInfo::ResponseStreamConnectionFailed {
                http_status_code: self.http_status_code_value(),
            },
            ChaosErr::RefreshTokenFailed(_) => ChaosErrorInfo::Unauthorized,
            ChaosErr::ProviderAuthMissing(err) => ChaosErrorInfo::ProviderAuthMissing {
                provider_id: err.provider_id.clone(),
                provider_name: err.provider_name.clone(),
                env_key: err.env_key.clone(),
                env_key_instructions: err.env_key_instructions.clone(),
                supports_oauth: err.supports_oauth,
            },
            ChaosErr::SessionConfiguredNotFirstEvent
            | ChaosErr::InternalServerError
            | ChaosErr::InternalAgentDied => ChaosErrorInfo::InternalServerError,
            ChaosErr::UnsupportedOperation(_)
            | ChaosErr::ProcessNotFound(_)
            | ChaosErr::AgentLimitReached { .. } => ChaosErrorInfo::BadRequest,
            ChaosErr::Sandbox(_) => ChaosErrorInfo::SandboxError,
            _ => ChaosErrorInfo::Other,
        }
    }

    pub fn to_error_event(&self, message_prefix: Option<String>) -> ErrorEvent {
        let error_message = self.to_string();
        let message: String = match message_prefix {
            Some(prefix) => format!("{prefix}: {error_message}"),
            None => error_message,
        };
        ErrorEvent {
            message,
            chaos_error_info: Some(self.to_chaos_ipc_error()),
        }
    }

    pub fn http_status_code_value(&self) -> Option<u16> {
        let http_status_code = match self {
            ChaosErr::RetryLimit(err) => Some(err.status),
            ChaosErr::UnexpectedStatus(err) => Some(err.status),
            ChaosErr::ConnectionFailed(_) => None,
            ChaosErr::ResponseStreamFailed(_) => None,
            _ => None,
        };
        http_status_code.as_ref().map(StatusCode::as_u16)
    }
}

pub fn get_error_message_ui(e: &ChaosErr) -> String {
    let message = match e {
        ChaosErr::Sandbox(SandboxErr::Denied { output, .. }) => {
            let aggregated = output.aggregated_output.text.trim();
            if !aggregated.is_empty() {
                output.aggregated_output.text.clone()
            } else {
                let stderr = output.stderr.text.trim();
                let stdout = output.stdout.text.trim();
                match (stderr.is_empty(), stdout.is_empty()) {
                    (false, false) => format!("{stderr}\n{stdout}"),
                    (false, true) => output.stderr.text.clone(),
                    (true, false) => output.stdout.text.clone(),
                    (true, true) => format!(
                        "command failed inside sandbox with exit code {}",
                        output.exit_code
                    ),
                }
            }
        }
        // Timeouts are not sandbox errors from a UX perspective; present them plainly
        ChaosErr::Sandbox(SandboxErr::Timeout { output }) => {
            format!(
                "error: command timed out after {} ms",
                output.duration.as_millis()
            )
        }
        _ => e.to_string(),
    };

    truncate_text(
        &message,
        TruncationPolicy::Bytes(ERROR_MESSAGE_UI_MAX_BYTES),
    )
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
