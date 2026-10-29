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

/// Per-model usage reported by Claude Code under `result.modelUsage`.
///
/// Claude Code keys this map by the model id it actually called (a dated id or
/// a capability alias), which can differ from the requested slug, and helper
/// models used on sidechains get their own entries. Only the fields Chaos
/// reads are declared; unknown fields are ignored.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Context window of that model in tokens, when Claude Code reports it.
    #[serde(default)]
    pub context_window: Option<u64>,
}

/// Model id to usage, as reported under `result.modelUsage`.
pub type ModelUsageMap = std::collections::BTreeMap<String, ModelUsage>;

/// The context window Claude Code reported for the model that served the
/// main thread.
///
/// `served_model` is the `message.model` of the last main-thread assistant
/// message, which is how Claude Code names the model that produced the reply;
/// the requested slug is the only fallback. Nothing is inferred from usage
/// volume: a sidechain helper can dominate a turn's tokens without carrying
/// the conversation, and a primary model without a reported window must not
/// inherit a helper's.
pub fn observed_context_window(
    model_usage: &ModelUsageMap,
    served_model: Option<&str>,
    requested_model: &str,
) -> Option<u64> {
    let window_for = |model: &str| {
        model_usage
            .get(model)
            .and_then(|usage| usage.context_window)
    };
    served_model
        .and_then(window_for)
        .or_else(|| window_for(requested_model))
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
    Assistant {
        message: Value,
        /// Set on sidechain (sub-agent) messages; `None` on the main thread.
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },

    /// The turn completed.
    #[serde(rename = "result")]
    Result {
        result: Value,
        #[serde(default)]
        subtype: Option<String>,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        usage: Option<Usage>,
        #[serde(default, rename = "modelUsage")]
        model_usage: Option<ModelUsageMap>,
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
mod tests;
