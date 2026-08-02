//! Stream-json control protocol types.
//!
//! This mirrors the wire format used by Claude Code's `--input-format stream-json`
//! and `--output-format stream-json` modes.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Messages sent TO Claude Code (stdin)
// ---------------------------------------------------------------------------

/// A user message sent to Claude Code.
#[derive(Debug, Serialize)]
pub struct UserMessage {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub message: UserMessageContent,
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct UserMessageContent {
    pub role: &'static str,
    pub content: String,
}

impl UserMessage {
    pub fn new(content: String, session_id: String) -> Self {
        Self {
            msg_type: "user",
            message: UserMessageContent {
                role: "user",
                content,
            },
            parent_tool_use_id: None,
            session_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Control protocol
// ---------------------------------------------------------------------------

/// A control request envelope (bidirectional).
///
/// Both sides (SDK and Claude Code) can send control requests.
/// The recipient sends back a `ControlResponse` with the matching `request_id`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ControlRequest {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub request_id: String,
    pub request: Value,
}

/// A control response envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct ControlResponse {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub response: ControlResponseBody,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ControlResponseBody {
    pub subtype: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    pub fn success(request_id: String, response: Value) -> Self {
        Self {
            msg_type: "control_response".to_string(),
            response: ControlResponseBody {
                subtype: "success".to_string(),
                request_id,
                response: Some(response),
                error: None,
            },
        }
    }

    pub fn error(request_id: String, error: String) -> Self {
        Self {
            msg_type: "control_response".to_string(),
            response: ControlResponseBody {
                subtype: "error".to_string(),
                request_id,
                response: None,
                error: Some(error),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Parsed message from Claude Code's stdout
// ---------------------------------------------------------------------------

/// Token usage reported by Claude Code for the primary result model.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// A parsed message from Claude Code's stream-json output.
///
/// The `type` field is used for tag-based deserialization but some message
/// types share the same tag (e.g. `control_request` comes from Claude Code
/// as an incoming request). We parse generically first, then match.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    /// An assistant response chunk.
    #[serde(rename = "assistant")]
    Assistant { message: Value },

    /// The turn completed.
    #[serde(rename = "result")]
    Result {
        result: Value,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        usage: Option<Usage>,
    },

    /// A system message (e.g., init, rate limit, error).
    ///
    /// Claude Code emits these with varying shapes — some have a `message`
    /// field, others (e.g. the `init` subtype) do not.  Accept any struct
    /// body via flatten so deserialization never fails on an unknown layout.
    #[serde(rename = "system")]
    System {
        #[serde(default)]
        message: Option<Value>,
        #[serde(flatten)]
        extra: serde_json::Map<String, Value>,
    },

    /// A control response from Claude Code to our control request.
    #[serde(rename = "control_response")]
    ControlResponse { response: Value },

    /// A control request from Claude Code (hook callback, permission, MCP).
    #[serde(rename = "control_request")]
    ControlRequestIncoming { request_id: String, request: Value },

    /// A cancel request for a pending control request.
    #[serde(rename = "control_cancel_request")]
    ControlCancelRequest { request_id: String },

    /// Catch-all for unknown message types.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Request builders
// ---------------------------------------------------------------------------

/// Build the `initialize` control request body.
pub fn initialize_request() -> Value {
    serde_json::json!({
        "subtype": "initialize",
        "hooks": null
    })
}

/// Build a control request envelope.
pub fn control_request_envelope(request_id: &str, request: Value) -> Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request
    })
}

#[cfg(test)]
mod tests {
    use super::Message;
    use serde_json::json;

    #[test]
    fn result_preserves_usage() {
        let message: Message = serde_json::from_value(json!({
            "type": "result",
            "subtype": "success",
            "result": "ok",
            "session_id": "session-1",
            "total_cost_usd": 0.0,
            "usage": {
                "input_tokens": 11,
                "cache_creation_input_tokens": 13,
                "cache_read_input_tokens": 17,
                "output_tokens": 19,
                "service_tier": "standard"
            },
            "modelUsage": {
                "claude-test": {
                    "inputTokens": 11,
                    "cacheCreationInputTokens": 13,
                    "cacheReadInputTokens": 17,
                    "outputTokens": 19
                }
            }
        }))
        .expect("result should deserialize");

        let Message::Result { usage, .. } = message else {
            panic!("expected result message");
        };

        let usage = usage.expect("usage should be present");
        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.cache_creation_input_tokens, 13);
        assert_eq!(usage.cache_read_input_tokens, 17);
        assert_eq!(usage.output_tokens, 19);
    }

    #[test]
    fn result_without_usage_remains_unknown() {
        let message: Message = serde_json::from_value(json!({
            "type": "result",
            "result": "ok",
            "session_id": "session-1",
            "total_cost_usd": 0.0
        }))
        .expect("historical result should deserialize");

        let Message::Result { usage, .. } = message else {
            panic!("expected result message");
        };

        assert_eq!(usage, None);
    }

    #[test]
    fn result_usage_ignores_unknown_nested_fields() {
        let message: Message = serde_json::from_value(json!({
            "type": "result",
            "result": "ok",
            "usage": {
                "input_tokens": 11,
                "cache_creation_input_tokens": 13,
                "cache_read_input_tokens": 17,
                "output_tokens": 19,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 13,
                    "ephemeral_1h_input_tokens": 0
                },
                "server_tool_use": {
                    "web_search_requests": 1,
                    "web_fetch_requests": 0
                },
                "service_tier": "standard"
            }
        }))
        .expect("future usage fields should not break deserialization");

        let Message::Result { usage, .. } = message else {
            panic!("expected result message");
        };

        assert!(usage.is_some());
    }

    #[test]
    fn error_result_remains_parseable_without_usage() {
        let message: Message = serde_json::from_value(json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "result": "execution failed",
            "session_id": "session-1"
        }))
        .expect("error result should deserialize");

        let Message::Result { usage, .. } = message else {
            panic!("expected result message");
        };

        assert_eq!(usage, None);
    }
}
