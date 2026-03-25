//! Provider-neutral tool definitions.
//!
//! The ABI knows two kinds of tools:
//! - **Function**: standard tools with JSON Schema parameters (maps to both
//!   OpenAI function tools and Anthropic tool definitions).
//! - **Freeform**: custom-format tools (XML, etc.) that don't use JSON Schema.
//!
//! Provider-specific tool types (OpenAI's `local_shell`, `web_search`,
//! `image_generation`, `tool_search`) are NOT part of the ABI. Those are
//! injected by the OpenAI adapter from its own capabilities.

use serde_json::Value;

/// A provider-neutral tool definition.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolDef {
    /// A standard function tool with JSON Schema parameters.
    Function(FunctionToolDef),

    /// A custom-format tool (non-JSON-Schema).
    Freeform(FreeformToolDef),
}

/// A function tool with a name, description, and JSON Schema parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionToolDef {
    pub name: String,
    pub description: String,

    /// JSON Schema describing the tool's parameters.
    /// Stored as a `Value` — the adapter wraps it in the provider's format.
    pub parameters: Value,

    /// Whether the provider should strictly validate the schema.
    pub strict: bool,
}

/// A custom-format tool that uses non-JSON-Schema definitions.
#[derive(Debug, Clone, PartialEq)]
pub struct FreeformToolDef {
    pub name: String,
    pub description: String,
    /// Format identifier (e.g. `"xml"`).
    pub format_type: String,
    /// Syntax specification.
    pub syntax: String,
    /// Full format definition text.
    pub definition: String,
}
