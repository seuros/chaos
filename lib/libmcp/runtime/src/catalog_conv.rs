//! Conversion functions from `mcp_guest` protocol types to `chaos_traits::catalog` types.
//!
//! These live here so that `chaos-kern`'s Catalog never imports `mcp_guest` directly.

use chaos_traits::catalog::{
    CatalogPrompt, CatalogPromptArgument, CatalogResource, CatalogResourceTemplate, CatalogTool,
};

use crate::manager::ToolInfo;

/// Convert the runtime's `ToolInfo` wrapper into a `CatalogTool`.
pub fn mcp_tool_info_to_catalog_tool(info: &ToolInfo) -> CatalogTool {
    CatalogTool {
        name: info.tool_name.clone(),
        description: info.tool.description.clone().unwrap_or_default(),
        input_schema: info.tool.input_schema.clone(),
        annotations: info
            .tool
            .annotations
            .as_ref()
            .and_then(|a| serde_json::to_value(a).ok()),
        read_only_hint: info.tool.annotations.as_ref().and_then(|a| a.read_only_hint),
        supports_parallel_tool_calls: true,
    }
}

/// Convert a `mcp_guest` `ResourceInfo` into a `CatalogResource`.
pub fn mcp_resource_to_catalog(info: &mcp_guest::protocol::ResourceInfo) -> CatalogResource {
    CatalogResource {
        uri: info.uri.clone(),
        name: info.name.clone(),
        description: info.description.clone(),
        mime_type: info.mime_type.clone(),
    }
}

/// Convert a `mcp_guest` `ResourceTemplateInfo` into a `CatalogResourceTemplate`.
pub fn mcp_resource_template_to_catalog(
    info: &mcp_guest::protocol::ResourceTemplateInfo,
) -> CatalogResourceTemplate {
    CatalogResourceTemplate {
        uri_template: info.uri_template.clone(),
        name: info.name.clone(),
        description: info.description.clone(),
        mime_type: info.mime_type.clone(),
    }
}

/// Convert a `mcp_guest` `PromptInfo` into a `CatalogPrompt`.
pub fn mcp_prompt_to_catalog(info: &mcp_guest::protocol::PromptInfo) -> CatalogPrompt {
    CatalogPrompt {
        name: info.name.clone(),
        description: info.description.clone(),
        arguments: info
            .arguments
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|a| CatalogPromptArgument {
                name: a.name.clone(),
                description: a.description.clone(),
                required: a.required.unwrap_or(false),
            })
            .collect(),
    }
}
