//! Centralized capability catalog for the kernel.
//!
//! Static modules register via `inventory::submit!` in their own crates.
//! MCP servers register dynamically at runtime. All consumers query
//! the same `Catalog` instance on `SessionServices`.

use crate::mcp_connection_manager::ToolInfo as McpToolInfo;
use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogTool;

/// Identifies who registered a catalog entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CatalogSource {
    /// Static module registered via `inventory` (e.g. "arsenal", "cron").
    Module(String),
    /// Dynamic MCP server.
    Mcp(String),
}

/// In-memory registry of capabilities (tools, resources, templates, prompts).
///
/// Currently tracks tools only. Resources, templates, and prompts will be
/// added when MCP integration lands (Step 5).
pub(crate) struct Catalog {
    tools: Vec<(CatalogSource, CatalogTool)>,
}

impl Catalog {
    /// Discover all statically registered modules via `inventory` and build
    /// the initial catalog. Call once at session boot.
    pub(crate) fn from_inventory() -> Self {
        let mut catalog = Self { tools: Vec::new() };

        for reg in inventory::iter::<CatalogRegistration> {
            let source = CatalogSource::Module(reg.name.to_string());
            for tool in (reg.tools)() {
                catalog.tools.push((source.clone(), tool));
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

    /// Remove all entries for an MCP server (disconnect or list_changed).
    pub(crate) fn unregister_mcp(&mut self, server: &str) {
        let mcp_source = CatalogSource::Mcp(server.to_string());
        self.tools.retain(|(s, _)| *s != mcp_source);
    }

    /// Remove all MCP entries (used on full MCP refresh).
    pub(crate) fn clear_all_mcp(&mut self) {
        self.tools
            .retain(|(s, _)| !matches!(s, CatalogSource::Mcp(_)));
    }

    /// All registered tools.
    pub(crate) fn tools(&self) -> &[(CatalogSource, CatalogTool)] {
        &self.tools
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
}
