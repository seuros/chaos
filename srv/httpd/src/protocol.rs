use serde::{Deserialize, Serialize};
use serde_json::Value;

use chaos_ipc::ProcessId;

/// Incoming trigger request body.
#[derive(Debug, Deserialize)]
pub struct TriggerRequest {
    /// The prompt to submit to Chaos. Also accepts `prompt` for shim compat.
    #[serde(alias = "prompt")]
    pub request: Option<String>,

    /// Caller-owned session identifier. Also accepts `session_id`.
    #[serde(alias = "session_id")]
    pub caller_session_id: Option<String>,

    /// Correlation id for upstream systems and logs.
    pub conversation_id: Option<String>,

    /// Actor identifier for audit logging.
    pub requested_by: Option<String>,

    /// Opaque JSON object recorded in tracing spans and persisted with the
    /// session; never interpreted for authorization.
    pub metadata: Option<Value>,

    /// Per-request model selection is forbidden. If present, reject with 400.
    #[serde(default)]
    pub model: Option<String>,
}

/// Token usage snapshot mirroring `TokenCountEvent.info`.
#[derive(Debug, Clone, Serialize)]
pub struct TokenUsageResponse {
    pub total_token_usage: TokenUsageEntry,
    pub last_token_usage: TokenUsageEntry,
    pub model_context_window: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenUsageEntry {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

/// Successful trigger response.
#[derive(Debug, Serialize)]
pub struct TriggerResponse {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub process_id: String,
    pub result: String,
    pub usage: Option<TokenUsageResponse>,
}

/// JSON error response body.
#[derive(Debug, Serialize)]
pub struct ApiErrorResponse {
    pub status: &'static str,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

impl ApiErrorResponse {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: "error",
            error: message.into(),
            process_id: None,
            caller_session_id: None,
            conversation_id: None,
        }
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self {
            status: "timeout",
            error: message.into(),
            process_id: None,
            caller_session_id: None,
            conversation_id: None,
        }
    }

    pub fn with_process_id(mut self, id: ProcessId) -> Self {
        self.process_id = Some(id.to_string());
        self
    }

    pub fn with_caller_fields(
        mut self,
        caller_session_id: Option<String>,
        conversation_id: Option<String>,
    ) -> Self {
        self.caller_session_id = caller_session_id;
        self.conversation_id = conversation_id;
        self
    }
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_request_accepts_request_field() {
        let json = r#"{"request": "hello world"}"#;
        let req: TriggerRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.request.as_deref(), Some("hello world"));
    }

    #[test]
    fn trigger_request_accepts_prompt_alias() {
        let json = r#"{"prompt": "hello world"}"#;
        let req: TriggerRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.request.as_deref(), Some("hello world"));
    }

    #[test]
    fn trigger_request_accepts_session_id_alias() {
        let json = r#"{"request": "x", "session_id": "abc-123"}"#;
        let req: TriggerRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.caller_session_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn trigger_request_rejects_model_field_detected() {
        let json = r#"{"request": "x", "model": "gpt-5"}"#;
        let req: TriggerRequest = serde_json::from_str(json).unwrap();
        assert!(req.model.is_some());
    }

    #[test]
    fn empty_request_is_none() {
        let json = r#"{}"#;
        let req: TriggerRequest = serde_json::from_str(json).unwrap();
        assert!(req.request.is_none());
    }

    #[test]
    fn error_response_serializes_consistently() {
        let err = ApiErrorResponse::error("unauthorized");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"], "unauthorized");
        // Optional fields should be absent, not null
        assert!(json.get("process_id").is_none());
    }

    #[test]
    fn timeout_response_has_timeout_status() {
        let err = ApiErrorResponse::timeout("deadline exceeded");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["status"], "timeout");
    }
}
