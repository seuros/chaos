//! Driver registration for tools, resources, templates, and prompts.
//!
//! Static modules self-register at link time via `inventory::submit!`.
//! The kernel discovers them at boot via `inventory::iter::<CatalogRegistration>`.
//! MCP servers register dynamically at runtime through the kernel's `Catalog`.

use serde_json::Value;

/// A static module registration. Modules submit these via `inventory::submit!`.
/// The kernel discovers them at startup via `inventory::iter`.
pub struct CatalogRegistration {
    /// Module name (e.g. "arsenal", "cron").
    pub name: &'static str,
    /// Returns all tools this module provides.
    pub tools: fn() -> Vec<CatalogTool>,
    /// Returns all resources this module provides.
    pub resources: fn() -> Vec<CatalogResource>,
    /// Returns all resource templates this module provides.
    pub resource_templates: fn() -> Vec<CatalogResourceTemplate>,
    /// Returns all prompts this module provides.
    pub prompts: fn() -> Vec<CatalogPrompt>,
}

inventory::collect!(CatalogRegistration);

/// Tool metadata — enough to display and to build a ToolSpec.
pub struct CatalogTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Option<Value>,
}

/// Resource metadata.
pub struct CatalogResource {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// Resource template metadata.
pub struct CatalogResourceTemplate {
    pub uri_template: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// Prompt metadata.
pub struct CatalogPrompt {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<CatalogPromptArgument>,
}

/// A single argument for a prompt.
pub struct CatalogPromptArgument {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}
