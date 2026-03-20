//! Compatibility shim re-exporting types from `mcp-host` with convenience
//! aliases for the codex-mcp-server codebase.
//!
//! This module bridges the gap between rmcp's API surface and mcp-host's,
//! keeping the rest of the crate largely unchanged.

// --- Protocol types ---
pub use mcp_host::protocol::types::{
    CallToolRequestParams, CallToolResult, CancelledNotificationParams, ContentItem,
    Implementation, JsonRpcError, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse,
    ListToolsResult, ProgressNotificationParams, RequestId,
};

// --- Capabilities ---
pub use mcp_host::protocol::capabilities::{
    ElicitationCapability, InitializeRequest, InitializeResult, ServerCapabilities,
    ToolsCapability,
};
#[cfg(test)]
pub use mcp_host::protocol::capabilities::UrlElicitationCapability;

// --- Protocol version ---
pub use mcp_host::protocol::version::ProtocolVersion;

// --- Tool registry ---
pub use mcp_host::registry::tools::ToolInfo;

// ---------------------------------------------------------------------------
// Aliases bridging rmcp naming to mcp-host naming
// ---------------------------------------------------------------------------

/// In rmcp the error payload was called `ErrorData`; mcp-host calls it
/// `JsonRpcError`. Keep the old name around so call-sites read naturally.
pub type ErrorData = JsonRpcError;

/// rmcp had a dedicated `JsonObject` alias; mcp-host uses the serde type
/// directly.
pub type JsonObject = serde_json::Map<String, serde_json::Value>;

// ---------------------------------------------------------------------------
// Convenience helpers matching rmcp's 3-arg constructors
// ---------------------------------------------------------------------------

/// Extension trait adding rmcp-compatible constructors to [`JsonRpcError`].
///
/// rmcp's `ErrorData::new(code, msg, data)` took an `Option<Value>` as the
/// third argument. mcp-host's `JsonRpcError::new(code, msg)` is 2-arg with a
/// chainable `.with_data()`. This trait bridges the gap.
pub trait ErrorDataExt {
    fn with_optional_data(self, data: Option<serde_json::Value>) -> Self;
}

impl ErrorDataExt for JsonRpcError {
    fn with_optional_data(self, data: Option<serde_json::Value>) -> Self {
        match data {
            Some(d) => self.with_data(d),
            None => self,
        }
    }
}
