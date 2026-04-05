//! Driver registration for tools, resources, templates, and prompts.
//!
//! Static modules self-register at link time via `inventory::submit!`.
//! The kernel discovers them at boot via `inventory::iter::<CatalogRegistration>`.
//! MCP servers register dynamically at runtime through the kernel's `Catalog`.

use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub type CatalogToolDriverFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CatalogToolResult, String>> + Send + 'a>>;

pub trait CatalogToolDriver: Send + Sync {
    fn call_tool(&self, request: CatalogToolRequest) -> CatalogToolDriverFuture<'_>;
}

pub type CatalogToolDriverFactory = fn() -> Arc<dyn CatalogToolDriver>;

#[derive(Debug, Clone)]
pub struct CatalogToolRequest {
    pub tool_name: String,
    pub arguments: Value,
    pub cwd: PathBuf,
    pub sqlite_home: PathBuf,
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub struct CatalogToolResult {
    pub output: String,
    pub success: Option<bool>,
}

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
    /// Optional driver factory for executing the module's catalog tools.
    pub tool_driver: Option<CatalogToolDriverFactory>,
}

inventory::collect!(CatalogRegistration);

/// Tool metadata — enough to display and to build a ToolSpec.
pub struct CatalogTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Option<Value>,
    pub read_only_hint: Option<bool>,
    pub supports_parallel_tool_calls: bool,
}

/// Convert a generated MCP `ToolInfo` into the kernel's lighter-weight catalog shape.
pub fn tool_info_to_catalog_tool(info: mcp_host::prelude::ToolInfo) -> CatalogTool {
    CatalogTool {
        read_only_hint: info.annotations.as_ref().and_then(|a| a.read_only_hint),
        supports_parallel_tool_calls: true,
        name: info.name,
        description: info.description.unwrap_or_default(),
        input_schema: info.input_schema,
        annotations: info.annotations.and_then(|a| serde_json::to_value(a).ok()),
    }
}

/// Convert a collection of generated `ToolInfo` values into catalog entries.
pub fn tool_infos_to_catalog_tools<I>(infos: I) -> Vec<CatalogTool>
where
    I: IntoIterator<Item = mcp_host::prelude::ToolInfo>,
{
    infos.into_iter().map(tool_info_to_catalog_tool).collect()
}

/// Convert a collection of generated `ToolInfo` values into catalog entries
/// with an explicit parallel-tool-calls flag.
pub fn tool_infos_to_catalog_tools_with_parallel<I>(
    infos: I,
    supports_parallel_tool_calls: bool,
) -> Vec<CatalogTool>
where
    I: IntoIterator<Item = mcp_host::prelude::ToolInfo>,
{
    tool_infos_to_catalog_tools(infos)
        .into_iter()
        .map(|mut tool| {
            tool.supports_parallel_tool_calls = supports_parallel_tool_calls;
            tool
        })
        .collect()
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

/// Sink for dynamic MCP catalog registration.
///
/// `chaos-mcp-runtime` holds an `Arc<dyn McpCatalogSink>` so that the
/// connection manager can update the kernel's tool/resource registry without
/// importing `chaos-kern` types.  The kernel's `Catalog` implements this trait.
pub trait McpCatalogSink: Send + Sync {
    fn register_mcp_tools(&self, server: &str, tools: Vec<CatalogTool>);
    fn register_mcp_resources(
        &self,
        server: &str,
        resources: Vec<CatalogResource>,
        templates: Vec<CatalogResourceTemplate>,
    );
    fn register_mcp_prompts(&self, server: &str, prompts: Vec<CatalogPrompt>);
    fn unregister_mcp(&self, server: &str);
    fn unregister_mcp_resources(&self, server: &str);
    fn unregister_mcp_prompts(&self, server: &str);
    fn clear_all_mcp(&self);
}
