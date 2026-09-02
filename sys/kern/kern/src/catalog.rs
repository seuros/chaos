//! Centralized capability catalog for the kernel.
//!
//! Static modules register via `inventory::submit!` in their own crates.
//! MCP servers register dynamically at runtime. All consumers query
//! the same `Catalog` instance on `SessionServices`.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::RwLock as StdRwLock;

use chaos_traits::McpCatalogSink;
use chaos_traits::catalog::CatalogPrompt;
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
        use std::collections::HashSet;
        let mut catalog = Self {
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
        };

        // Deduplicate by module name — inventory can yield the same static
        // registration twice when a crate is linked through multiple paths
        // (common in test binaries).
        let mut seen = HashSet::new();
        for reg in inventory::iter::<CatalogRegistration> {
            if !seen.insert(reg.name) {
                continue;
            }
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

    /// Remove only tools for an MCP server (tools/list_changed).
    pub(crate) fn unregister_mcp_tools(&mut self, server: &str) {
        let mcp_source = CatalogSource::Mcp(server.to_string());
        self.tools.retain(|(s, _)| *s != mcp_source);
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

    pub(crate) fn resources(&self) -> &[(CatalogSource, CatalogResource)] {
        &self.resources
    }

    pub(crate) fn resource_templates(&self) -> &[(CatalogSource, CatalogResourceTemplate)] {
        &self.resource_templates
    }

    #[cfg(test)]
    pub(crate) fn prompts(&self) -> &[(CatalogSource, CatalogPrompt)] {
        &self.prompts
    }
}

/// Thread-safe wrapper around [`Catalog`] that implements [`McpCatalogSink`].
///
/// Wraps the catalog in a `RwLock` so that `McpConnectionManager` can hold
/// an `Arc<dyn McpCatalogSink>` without knowing about the kernel's `Catalog` type.
pub(crate) struct CatalogSink(pub(crate) StdRwLock<Catalog>);

impl CatalogSink {
    pub(crate) fn new(catalog: Catalog) -> Self {
        Self(StdRwLock::new(catalog))
    }

    pub(crate) fn read(
        &self,
    ) -> Result<
        std::sync::RwLockReadGuard<'_, Catalog>,
        std::sync::PoisonError<std::sync::RwLockReadGuard<'_, Catalog>>,
    > {
        self.0.read()
    }

    pub(crate) fn write(
        &self,
    ) -> Result<
        std::sync::RwLockWriteGuard<'_, Catalog>,
        std::sync::PoisonError<std::sync::RwLockWriteGuard<'_, Catalog>>,
    > {
        self.0.write()
    }
}

impl McpCatalogSink for CatalogSink {
    fn register_mcp_tools(&self, server: &str, tools: Vec<CatalogTool>) {
        if let Ok(mut c) = self.0.write() {
            c.register_mcp_tools(server, tools);
        }
    }

    fn register_mcp_resources(
        &self,
        server: &str,
        resources: Vec<CatalogResource>,
        templates: Vec<CatalogResourceTemplate>,
    ) {
        if let Ok(mut c) = self.0.write() {
            c.register_mcp_resources(server, resources);
            c.register_mcp_resource_templates(server, templates);
        }
    }

    fn register_mcp_prompts(&self, server: &str, prompts: Vec<CatalogPrompt>) {
        if let Ok(mut c) = self.0.write() {
            c.register_mcp_prompts(server, prompts);
        }
    }

    fn unregister_mcp(&self, server: &str) {
        if let Ok(mut c) = self.0.write() {
            c.unregister_mcp(server);
        }
    }

    fn unregister_mcp_tools(&self, server: &str) {
        if let Ok(mut c) = self.0.write() {
            c.unregister_mcp_tools(server);
        }
    }

    fn unregister_mcp_resources(&self, server: &str) {
        if let Ok(mut c) = self.0.write() {
            c.unregister_mcp_resources(server);
        }
    }

    fn unregister_mcp_prompts(&self, server: &str) {
        if let Ok(mut c) = self.0.write() {
            c.unregister_mcp_prompts(server);
        }
    }

    fn clear_all_mcp(&self) {
        if let Ok(mut c) = self.0.write() {
            c.clear_all_mcp();
        }
    }
}

enum CatalogMutation {
    RegisterTools {
        server: String,
        tools: Vec<CatalogTool>,
    },
    RegisterResources {
        server: String,
        resources: Vec<CatalogResource>,
        templates: Vec<CatalogResourceTemplate>,
    },
    RegisterPrompts {
        server: String,
        prompts: Vec<CatalogPrompt>,
    },
    Unregister {
        server: String,
    },
    UnregisterTools {
        server: String,
    },
    UnregisterResources {
        server: String,
    },
    UnregisterPrompts {
        server: String,
    },
    ClearAll,
}

impl CatalogMutation {
    fn apply(self, catalog: &mut Catalog) {
        match self {
            Self::RegisterTools { server, tools } => {
                catalog.register_mcp_tools(&server, tools);
            }
            Self::RegisterResources {
                server,
                resources,
                templates,
            } => {
                catalog.register_mcp_resources(&server, resources);
                catalog.register_mcp_resource_templates(&server, templates);
            }
            Self::RegisterPrompts { server, prompts } => {
                catalog.register_mcp_prompts(&server, prompts);
            }
            Self::Unregister { server } => catalog.unregister_mcp(&server),
            Self::UnregisterTools { server } => catalog.unregister_mcp_tools(&server),
            Self::UnregisterResources { server } => {
                catalog.unregister_mcp_resources(&server);
            }
            Self::UnregisterPrompts { server } => {
                catalog.unregister_mcp_prompts(&server);
            }
            Self::ClearAll => catalog.clear_all_mcp(),
        }
    }
}

enum McpCatalogGateState {
    Staging(Vec<CatalogMutation>),
    Active,
    Retired,
}

/// Generation-scoped MCP catalog sink.
///
/// A newly-created MCP manager receives a staging gate, so startup and
/// list-changed callbacks cannot mutate the live catalog before the registry
/// actor commits that generation. At cutover, the registry retires the old
/// gate, installs the new generation's complete tool snapshot, replays any
/// staged callbacks, and activates forwarding while dispatch remains paused in
/// the registry mailbox.
pub(crate) struct McpCatalogGate {
    live: Arc<CatalogSink>,
    state: StdMutex<McpCatalogGateState>,
}

impl McpCatalogGate {
    pub(crate) fn staging(live: Arc<CatalogSink>) -> Self {
        Self {
            live,
            state: StdMutex::new(McpCatalogGateState::Staging(Vec::new())),
        }
    }

    pub(crate) fn activate(&self, tools: Vec<(String, CatalogTool)>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let McpCatalogGateState::Staging(mutations) = &mut *state else {
            return;
        };
        let staged = std::mem::take(mutations);
        {
            let mut catalog = self
                .live
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            catalog.clear_all_mcp();
            for (server, tool) in tools {
                catalog.register_mcp_tools(&server, vec![tool]);
            }
            for mutation in staged {
                mutation.apply(&mut catalog);
            }
        }
        *state = McpCatalogGateState::Active;
    }

    pub(crate) fn retire(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = McpCatalogGateState::Retired;
    }

    fn submit(&self, mutation: CatalogMutation) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &mut *state {
            McpCatalogGateState::Staging(mutations) => mutations.push(mutation),
            McpCatalogGateState::Active => {
                let mut catalog = self
                    .live
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                mutation.apply(&mut catalog);
            }
            McpCatalogGateState::Retired => {}
        }
    }
}

impl McpCatalogSink for McpCatalogGate {
    fn register_mcp_tools(&self, server: &str, tools: Vec<CatalogTool>) {
        self.submit(CatalogMutation::RegisterTools {
            server: server.to_string(),
            tools,
        });
    }

    fn register_mcp_resources(
        &self,
        server: &str,
        resources: Vec<CatalogResource>,
        templates: Vec<CatalogResourceTemplate>,
    ) {
        self.submit(CatalogMutation::RegisterResources {
            server: server.to_string(),
            resources,
            templates,
        });
    }

    fn register_mcp_prompts(&self, server: &str, prompts: Vec<CatalogPrompt>) {
        self.submit(CatalogMutation::RegisterPrompts {
            server: server.to_string(),
            prompts,
        });
    }

    fn unregister_mcp(&self, server: &str) {
        self.submit(CatalogMutation::Unregister {
            server: server.to_string(),
        });
    }

    fn unregister_mcp_tools(&self, server: &str) {
        self.submit(CatalogMutation::UnregisterTools {
            server: server.to_string(),
        });
    }

    fn unregister_mcp_resources(&self, server: &str) {
        self.submit(CatalogMutation::UnregisterResources {
            server: server.to_string(),
        });
    }

    fn unregister_mcp_prompts(&self, server: &str) {
        self.submit(CatalogMutation::UnregisterPrompts {
            server: server.to_string(),
        });
    }

    fn clear_all_mcp(&self) {
        self.submit(CatalogMutation::ClearAll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_traits::catalog::CatalogPromptArgument;
    use serde_json::json;

    #[test]
    fn from_inventory_discovers_static_modules() {
        let catalog = Catalog::from_inventory();
        assert!(
            catalog
                .tools()
                .iter()
                .any(|(s, _)| *s == CatalogSource::Module("arsenal".to_string())),
            "arsenal should register at least one tool"
        );

        assert!(
            catalog
                .tools()
                .iter()
                .any(|(s, _)| *s == CatalogSource::Module("cron".to_string())),
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
                read_only_hint: None,
                supports_parallel_tool_calls: true,
            }],
        );
        assert_eq!(catalog.tools().len(), initial_count + 1);

        catalog.unregister_mcp("test-server");
        assert_eq!(catalog.tools().len(), initial_count);
    }

    #[test]
    fn unregister_mcp_tools_preserves_other_capability_types() {
        let mut catalog = Catalog::from_inventory();
        let initial_tool_count = catalog.tools().len();
        let initial_resource_count = catalog.resources().len();
        let initial_template_count = catalog.resource_templates().len();
        let initial_prompt_count = catalog.prompts().len();

        catalog.register_mcp_tools(
            "test-server",
            vec![CatalogTool {
                name: "test_tool".to_string(),
                description: "A test tool".to_string(),
                input_schema: json!({"type": "object"}),
                annotations: None,
                read_only_hint: None,
                supports_parallel_tool_calls: true,
            }],
        );
        catalog.register_mcp_resources(
            "test-server",
            vec![CatalogResource {
                uri: "test://resource".to_string(),
                name: "resource".to_string(),
                description: None,
                mime_type: None,
            }],
        );
        catalog.register_mcp_resource_templates(
            "test-server",
            vec![CatalogResourceTemplate {
                uri_template: "test://{name}".to_string(),
                name: "template".to_string(),
                description: None,
                mime_type: None,
            }],
        );
        catalog.register_mcp_prompts(
            "test-server",
            vec![CatalogPrompt {
                name: "prompt".to_string(),
                description: None,
                arguments: vec![],
            }],
        );

        catalog.unregister_mcp_tools("test-server");

        assert_eq!(catalog.tools().len(), initial_tool_count);
        assert_eq!(catalog.resources().len(), initial_resource_count + 1);
        assert_eq!(
            catalog.resource_templates().len(),
            initial_template_count + 1
        );
        assert_eq!(catalog.prompts().len(), initial_prompt_count + 1);
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
        let initial_template_count = catalog.resource_templates().len();

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
        assert_eq!(
            catalog.resource_templates().len(),
            initial_template_count + 1
        );

        catalog.unregister_mcp_resources("fs-server");
        assert!(catalog.resources().is_empty());
        assert_eq!(catalog.resource_templates().len(), initial_template_count);
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
                read_only_hint: None,
                supports_parallel_tool_calls: true,
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

    #[test]
    fn staged_mcp_catalog_gate_isolated_until_activation() {
        let live = Arc::new(CatalogSink::new(Catalog::from_inventory()));
        let initial_count = live
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tools()
            .len();
        let gate = McpCatalogGate::staging(Arc::clone(&live));

        gate.register_mcp_tools(
            "staged",
            vec![CatalogTool {
                name: "dynamic".to_string(),
                description: "dynamic".to_string(),
                input_schema: json!({"type": "object"}),
                annotations: None,
                read_only_hint: None,
                supports_parallel_tool_calls: true,
            }],
        );
        assert_eq!(
            live.read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tools()
                .len(),
            initial_count
        );

        gate.activate(Vec::new());
        assert_eq!(
            live.read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tools()
                .len(),
            initial_count + 1
        );

        gate.retire();
        gate.unregister_mcp("staged");
        assert_eq!(
            live.read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tools()
                .len(),
            initial_count + 1
        );
    }
}
