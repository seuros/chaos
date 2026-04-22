use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Chaos errors that we expose to clients.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ChaosErrorInfo {
    ContextWindowExceeded,
    UsageLimitExceeded,
    ServerOverloaded,
    HttpConnectionFailed {
        http_status_code: Option<u16>,
    },
    /// Failed to connect to the response SSE stream.
    ResponseStreamConnectionFailed {
        http_status_code: Option<u16>,
    },
    InternalServerError,
    Unauthorized,
    /// The selected provider requires credentials and none were found in any
    /// expected location (env var, cached login, bearer token). Surfaced before
    /// any network request so the client can show a login prompt instead of
    /// retrying a doomed 401.
    ProviderAuthMissing {
        provider_id: String,
        provider_name: String,
        env_key: Option<String>,
        env_key_instructions: Option<String>,
        supports_oauth: bool,
    },
    BadRequest,
    SandboxError,
    /// The response SSE stream disconnected in the middle of a turnbefore completion.
    ResponseStreamDisconnected {
        http_status_code: Option<u16>,
    },
    /// Reached the retry limit for responses.
    ResponseTooManyFailedAttempts {
        http_status_code: Option<u16>,
    },
    #[serde(rename = "process_rollback_failed")]
    #[ts(rename = "process_rollback_failed")]
    ProcessRollbackFailed,
    Other,
}

impl ChaosErrorInfo {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        match self {
            Self::ProcessRollbackFailed => false,
            Self::ContextWindowExceeded
            | Self::UsageLimitExceeded
            | Self::ServerOverloaded
            | Self::HttpConnectionFailed { .. }
            | Self::ResponseStreamConnectionFailed { .. }
            | Self::InternalServerError
            | Self::Unauthorized
            | Self::ProviderAuthMissing { .. }
            | Self::BadRequest
            | Self::SandboxError
            | Self::ResponseStreamDisconnected { .. }
            | Self::ResponseTooManyFailedAttempts { .. }
            | Self::Other => true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ErrorEvent {
    pub message: String,
    #[serde(default)]
    pub chaos_error_info: Option<ChaosErrorInfo>,
}

impl ErrorEvent {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        self.chaos_error_info
            .as_ref()
            .is_none_or(ChaosErrorInfo::affects_turn_status)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WarningEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamErrorEvent {
    pub message: String,
    #[serde(default)]
    pub chaos_error_info: Option<ChaosErrorInfo>,
    /// Optional details about the underlying stream failure (often the same
    /// human-readable message that is surfaced as the terminal error if retries
    /// are exhausted).
    #[serde(default)]
    pub additional_details: Option<String>,
}
