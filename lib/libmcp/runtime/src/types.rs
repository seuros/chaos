//! Kernel-facing domain types — no `mcp_guest` types in this module's public API.
//!
//! All conversions from `mcp_guest` protocol objects happen inside `chaos-mcp-runtime`
//! and never surface to `chaos-kern`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Describes a single tool exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub server_name: String,
    pub tool_name: String,
    /// Fully-qualified name: `<server_name>__<tool_name>`
    pub qualified_name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    pub annotations: Option<ToolAnnotationsSnapshot>,
}

/// Snapshot of MCP tool annotations, decoupled from the wire type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAnnotationsSnapshot {
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
    pub open_world_hint: Option<bool>,
    pub title: Option<String>,
}

/// Request to invoke an MCP tool.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub server_name: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Result of an MCP tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

/// A single content item returned by a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        mime_type: String,
    },
    Resource {
        uri: String,
        text: Option<String>,
        blob: Option<String>,
        mime_type: Option<String>,
    },
}

/// Describes a resource exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceDescriptor {
    pub server_name: String,
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// Result of reading an MCP resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceReadResult {
    pub contents: Vec<ResourceContent>,
}

/// A single content item from a resource read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    pub text: Option<String>,
    pub blob: Option<String>,
    pub mime_type: Option<String>,
}

/// Snapshot of available tools per server.
pub type ToolMap = HashMap<String, ToolDescriptor>;
