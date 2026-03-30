//! Centralized capability catalog for the kernel.
//!
//! Static modules register via `inventory::submit!` in their own crates.
//! MCP servers register dynamically at runtime. All consumers query
//! the same `Catalog` instance on `SessionServices`.

use crate::mcp_connection_manager::ToolInfo as McpToolInfo;
use chaos_traits::catalog::CatalogPrompt;
use chaos_traits::catalog::CatalogPromptArgument;
use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogResource;
use chaos_traits::catalog::CatalogResourceTemplate;
use chaos_traits::catalog::CatalogTool;

/// Identifies who registered a catalog entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CatalogSource {
    /// Static module registered via `inventory` (e.g. "arsenal", "cron").
    Module(String),
    /// Dynamic MCP server.
    Mcp(String),
}

/// In-memory registry of all capabilities: tools, resources, templates, prompts.
pub(crate) struct Catalog {
    tools: Vec<(CatalogSource, CatalogTool)>,
    resources: Vec<(CatalogSource, CatalogResource)>,
    resource_templates: Vec<(CatalogSource, CatalogResourceTemplate)>,
    prompts: Vec<(CatalogSource, CatalogPrompt)>,
}

impl Catalog {
    /// Discover all statically registered modules via `inventory` and build
    /// the initial catalog. Call once at session boot.
    pub(crate) fn from_inventory() -> Self {
        let mut catalog = Self {
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
        };

        for reg in inventory::iter::<CatalogRegistration> {
            let source = CatalogSource::Module(reg.name.to_string());
            for tool in (reg.tools)() {
                catalog.tools.push((source.clone(), tool));
            }
            for resource in (reg.resources)() {
                catalog.resources.push((source.clone(), resource));
            }
            for template in (reg.resource_templates)() {
                catalog.resource_templates.push((source.clone(), template));
            }
            for prompt in (reg.prompts)() {
                catalog.prompts.push((source.clone(), prompt));
            }
        }

        catalog
    }

    /// Register tools from a dynamic MCP server.
    pub(crate) fn register_mcp_tools(&mut self, server: &str, tools: Vec<CatalogTool>) {
        let source = CatalogSource::Mcp(server.to_string());
        for tool in tools {
            self.tools.push((source.clone(), tool));
        }
    }

    /// Register resources from a dynamic MCP server.
    pub(crate) fn register_mcp_resources(&mut self, server: &str, resources: Vec<CatalogResource>) {
        let source = CatalogSource::Mcp(server.to_string());
        for resource in resources {
            self.resources.push((source.clone(), resource));
        }
    }

    /// Register resource templates from a dynamic MCP server.
    pub(crate) fn register_mcp_resource_templates(
        &mut self,
        server: &str,
        templates: Vec<CatalogResourceTemplate>,
    ) {
        let source = CatalogSource::Mcp(server.to_string());
        for template in templates {
            self.resource_templates.push((source.clone(), template));
        }
    }

    /// Register prompts from a dynamic MCP server.
    pub(crate) fn register_mcp_prompts(&mut self, server: &str, prompts: Vec<CatalogPrompt>) {
        let source = CatalogSource::Mcp(server.to_string());
        for prompt in prompts {
            self.prompts.push((source.clone(), prompt));
        }
    }

    /// Remove all entries for an MCP server (disconnect or full refresh).
    pub(crate) fn unregister_mcp(&mut self, server: &str) {
        let mcp_source = CatalogSource::Mcp(server.to_string());
        self.tools.retain(|(s, _)| *s != mcp_source);
        self.resources.retain(|(s, _)| *s != mcp_source);
        self.resource_templates.retain(|(s, _)| *s != mcp_source);
        self.prompts.retain(|(s, _)| *s != mcp_source);
    }

    /// Remove only resources and templates for an MCP server (resources/list_changed).
    pub(crate) fn unregister_mcp_resources(&mut self, server: &str) {
        let mcp_source = CatalogSource::Mcp(server.to_string());
        self.resources.retain(|(s, _)| *s != mcp_source);
        self.resource_templates.retain(|(s, _)| *s != mcp_source);
    }

    /// Remove only prompts for an MCP server (prompts/list_changed).
    pub(crate) fn unregister_mcp_prompts(&mut self, server: &str) {
        let mcp_source = CatalogSource::Mcp(server.to_string());
        self.prompts.retain(|(s, _)| *s != mcp_source);
    }

    /// Remove all MCP entries (used on full MCP refresh).
    pub(crate) fn clear_all_mcp(&mut self) {
        self.tools
            .retain(|(s, _)| !matches!(s, CatalogSource::Mcp(_)));
        self.resources
            .retain(|(s, _)| !matches!(s, CatalogSource::Mcp(_)));
        self.resource_templates
            .retain(|(s, _)| !matches!(s, CatalogSource::Mcp(_)));
        self.prompts
            .retain(|(s, _)| !matches!(s, CatalogSource::Mcp(_)));
    }

    /// All registered tools.
    pub(crate) fn tools(&self) -> &[(CatalogSource, CatalogTool)] {
        &self.tools
    }

    /// All registered resources. Used by TUI /tools and IPC ListCatalog.
    #[allow(dead_code)]
    pub(crate) fn resources(&self) -> &[(CatalogSource, CatalogResource)] {
        &self.resources
    }

    /// All registered resource templates. Used by TUI /tools and IPC ListCatalog.
    #[allow(dead_code)]
    pub(crate) fn resource_templates(&self) -> &[(CatalogSource, CatalogResourceTemplate)] {
        &self.resource_templates
    }

    /// All registered prompts. Used by TUI /tools and IPC ListCatalog.
    #[allow(dead_code)]
    pub(crate) fn prompts(&self) -> &[(CatalogSource, CatalogPrompt)] {
        &self.prompts
    }
}

/// Convert MCP `ToolInfo` from the connection manager into a `CatalogTool`.
pub(crate) fn mcp_tool_info_to_catalog_tool(info: &McpToolInfo) -> CatalogTool {
    CatalogTool {
        name: info.tool_name.clone(),
        description: info.tool.description.clone().unwrap_or_default(),
        input_schema: info.tool.input_schema.clone(),
        annotations: info
            .tool
            .annotations
            .as_ref()
            .and_then(|a| serde_json::to_value(a).ok()),
    }
}

/// Convert MCP `ResourceInfo` into a `CatalogResource`.
pub(crate) fn mcp_resource_to_catalog(info: &mcp_guest::protocol::ResourceInfo) -> CatalogResource {
    CatalogResource {
        uri: info.uri.clone(),
        name: info.name.clone(),
        description: info.description.clone(),
        mime_type: info.mime_type.clone(),
    }
}

/// Convert MCP `ResourceTemplateInfo` into a `CatalogResourceTemplate`.
pub(crate) fn mcp_resource_template_to_catalog(
    info: &mcp_guest::protocol::ResourceTemplateInfo,
) -> CatalogResourceTemplate {
    CatalogResourceTemplate {
        uri_template: info.uri_template.clone(),
        name: info.name.clone(),
        description: info.description.clone(),
        mime_type: info.mime_type.clone(),
    }
}

/// Convert MCP `PromptInfo` into a `CatalogPrompt`.
pub(crate) fn mcp_prompt_to_catalog(info: &mcp_guest::protocol::PromptInfo) -> CatalogPrompt {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_inventory_discovers_static_modules() {
        let catalog = Catalog::from_inventory();
        let arsenal_tools: Vec<_> = catalog
            .tools()
            .iter()
            .filter(|(s, _)| *s == CatalogSource::Module("arsenal".to_string()))
            .collect();
        assert!(
            !arsenal_tools.is_empty(),
            "arsenal should register at least one tool"
        );

        let cron_tools: Vec<_> = catalog
            .tools()
            .iter()
            .filter(|(s, _)| *s == CatalogSource::Module("cron".to_string()))
            .collect();
        assert!(
            !cron_tools.is_empty(),
            "cron should register at least one tool"
        );
    }

    #[test]
    fn mcp_register_and_unregister() {
        let mut catalog = Catalog::from_inventory();
        let initial_count = catalog.tools().len();

        catalog.register_mcp_tools(
            "test-server",
            vec![CatalogTool {
                name: "test_tool".to_string(),
                description: "A test tool".to_string(),
                input_schema: json!({"type": "object"}),
                annotations: None,
            }],
        );
        assert_eq!(catalog.tools().len(), initial_count + 1);

        catalog.unregister_mcp("test-server");
        assert_eq!(catalog.tools().len(), initial_count);
    }

    #[test]
    fn unregister_mcp_does_not_touch_static_modules() {
        let mut catalog = Catalog::from_inventory();
        let initial_count = catalog.tools().len();

        catalog.unregister_mcp("arsenal");
        assert_eq!(
            catalog.tools().len(),
            initial_count,
            "unregister_mcp should not remove Module entries"
        );
    }

    #[test]
    fn mcp_resources_register_and_unregister() {
        let mut catalog = Catalog::from_inventory();
        assert!(catalog.resources().is_empty());

        catalog.register_mcp_resources(
            "fs-server",
            vec![CatalogResource {
                uri: "file:///tmp/data.csv".to_string(),
                name: "data.csv".to_string(),
                description: Some("Sample data".to_string()),
                mime_type: Some("text/csv".to_string()),
            }],
        );
        assert_eq!(catalog.resources().len(), 1);

        catalog.register_mcp_resource_templates(
            "fs-server",
            vec![CatalogResourceTemplate {
                uri_template: "file:///tmp/{name}".to_string(),
                name: "tmp files".to_string(),
                description: None,
                mime_type: None,
            }],
        );
        assert_eq!(catalog.resource_templates().len(), 1);

        catalog.unregister_mcp_resources("fs-server");
        assert!(catalog.resources().is_empty());
        assert!(catalog.resource_templates().is_empty());
    }

    #[test]
    fn mcp_prompts_register_and_unregister() {
        let mut catalog = Catalog::from_inventory();
        assert!(catalog.prompts().is_empty());

        catalog.register_mcp_prompts(
            "prompt-server",
            vec![CatalogPrompt {
                name: "summarize".to_string(),
                description: Some("Summarize text".to_string()),
                arguments: vec![CatalogPromptArgument {
                    name: "text".to_string(),
                    description: Some("Text to summarize".to_string()),
                    required: true,
                }],
            }],
        );
        assert_eq!(catalog.prompts().len(), 1);
        assert_eq!(catalog.prompts()[0].1.arguments.len(), 1);

        catalog.unregister_mcp_prompts("prompt-server");
        assert!(catalog.prompts().is_empty());
    }

    #[test]
    fn unregister_mcp_clears_all_capability_types() {
        let mut catalog = Catalog::from_inventory();

        catalog.register_mcp_tools(
            "full-server",
            vec![CatalogTool {
                name: "tool_a".to_string(),
                description: "A".to_string(),
                input_schema: json!({"type": "object"}),
                annotations: None,
            }],
        );
        catalog.register_mcp_resources(
            "full-server",
            vec![CatalogResource {
                uri: "res://a".to_string(),
                name: "a".to_string(),
                description: None,
                mime_type: None,
            }],
        );
        catalog.register_mcp_prompts(
            "full-server",
            vec![CatalogPrompt {
                name: "p".to_string(),
                description: None,
                arguments: vec![],
            }],
        );

        let tool_count = catalog.tools().len();
        catalog.unregister_mcp("full-server");

        // Tools should be back to static count, resources/prompts empty.
        assert_eq!(catalog.tools().len(), tool_count - 1);
        assert!(catalog.resources().is_empty());
        assert!(catalog.prompts().is_empty());
    }
}
