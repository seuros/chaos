use std::fmt;
use std::time::Duration;

use crate::dynamic_tools::DynamicToolCallOutputContentItem;
use crate::mcp::CallToolResult;
use crate::mcp::Resource as McpResource;
use crate::mcp::ResourceTemplate as McpResourceTemplate;
use crate::mcp::Tool as McpTool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct McpInvocation {
    /// Configured MCP server, absent for internal tool invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Bare internal tool name, or the name given by the MCP server.
    pub tool: String,
    /// Arguments to the tool call.
    pub arguments: Option<serde_json::Value>,
}

impl McpInvocation {
    pub fn display_name(&self) -> String {
        match &self.server {
            Some(server) => format!("{server}.{}", self.tool),
            None => self.tool.clone(),
        }
    }
}

#[cfg(test)]
mod invocation_tests {
    use super::*;

    #[test]
    fn internal_invocation_omits_server_and_prefix() {
        let invocation = McpInvocation {
            server: None,
            tool: "read_mcp_resource".into(),
            arguments: None,
        };
        let value = serde_json::to_value(&invocation).unwrap();
        assert!(value.get("server").is_none());
        assert_eq!(invocation.display_name(), "read_mcp_resource");
        assert_eq!(
            serde_json::from_value::<McpInvocation>(value).unwrap(),
            invocation
        );
        let schema = serde_json::to_value(schemars::schema_for!(McpInvocation)).unwrap();
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("server"))
        );
    }

    #[test]
    fn external_invocation_retains_server_and_prefix() {
        let invocation = McpInvocation {
            server: Some("external".into()),
            tool: "search".into(),
            arguments: None,
        };
        let value = serde_json::to_value(&invocation).unwrap();
        assert_eq!(value["server"], "external");
        assert_eq!(invocation.display_name(), "external.search");
        assert_eq!(
            serde_json::from_value::<McpInvocation>(value).unwrap(),
            invocation
        );
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct McpToolCallBeginEvent {
    /// Identifier so this can be paired with the McpToolCallEnd event.
    pub call_id: String,
    pub invocation: McpInvocation,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct McpToolCallEndEvent {
    /// Identifier for the corresponding McpToolCallBegin that finished.
    pub call_id: String,
    pub invocation: McpInvocation,
    pub duration: Duration,
    /// Result of the tool call. Note this could be an error.
    pub result: Result<CallToolResult, String>,
}

impl McpToolCallEndEvent {
    pub fn is_success(&self) -> bool {
        match &self.result {
            Ok(result) => !result.is_error.unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct DynamicToolCallResponseEvent {
    /// Identifier for the corresponding DynamicToolCallRequest.
    pub call_id: String,
    /// Turn ID that this dynamic tool call belongs to.
    pub turn_id: String,
    /// Dynamic tool name.
    pub tool: String,
    /// Dynamic tool call arguments.
    pub arguments: serde_json::Value,
    /// Dynamic tool response content items.
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    /// Whether the tool call succeeded.
    pub success: bool,
    /// Optional error text when the tool call failed before producing a response.
    pub error: Option<String>,
    /// The duration of the dynamic tool call.
    pub duration: Duration,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct McpStartupUpdateEvent {
    /// Server name being started.
    pub server: String,
    /// Current startup status.
    pub status: McpStartupStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum McpStartupStatus {
    Starting,
    Ready,
    Failed { error: String },
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub struct McpStartupCompleteEvent {
    pub ready: Vec<String>,
    pub failed: Vec<McpStartupFailure>,
    pub cancelled: Vec<String>,
}

/// Result of an explicit live MCP refresh request.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct McpServersRefreshedEvent {
    pub revision: u64,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    pub ready: Vec<String>,
    pub failed: Vec<McpStartupFailure>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct McpStartupFailure {
    pub server: String,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthStatus {
    Unsupported,
    NotLoggedIn,
    BearerToken,
    OAuth,
}

impl fmt::Display for McpAuthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            McpAuthStatus::Unsupported => "Unsupported",
            McpAuthStatus::NotLoggedIn => "Not logged in",
            McpAuthStatus::BearerToken => "Bearer token",
            McpAuthStatus::OAuth => "OAuth",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct McpListToolsResponseEvent {
    /// Fully qualified tool name -> tool definition.
    pub tools: std::collections::HashMap<String, McpTool>,
    /// Known resources grouped by server name.
    pub resources: std::collections::HashMap<String, Vec<McpResource>>,
    /// Known resource templates grouped by server name.
    pub resource_templates: std::collections::HashMap<String, Vec<McpResourceTemplate>>,
    /// Authentication status for each configured MCP server.
    pub auth_statuses: std::collections::HashMap<String, McpAuthStatus>,
}

/// A single tool entry in the all-tools response.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ToolSummary {
    /// Tool name for display in `/tools`. MCP names omit the model-only
    /// `mcp__<server>__` qualification.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Precomputed annotation labels for UI badges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotation_labels: Vec<String>,
    /// Optional structured tool annotations for UI rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
    /// Origin: "builtin", a catalog module, "dynamic", "halluacinate", or "mcp:<server>".
    pub source: String,
}

/// Response to `Op::ListAllTools`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AllToolsResponseEvent {
    pub tools: Vec<ToolSummary>,
}

/// Response payload for `Op::ListCustomPrompts`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListCustomPromptsResponseEvent {
    pub custom_prompts: Vec<crate::custom_prompts::CustomPrompt>,
}
