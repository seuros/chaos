use std::sync::Arc;
use std::sync::Mutex;

use chaos_traits::catalog::CatalogPrompt;
use chaos_traits::catalog::CatalogResource;
use chaos_traits::catalog::CatalogResourceTemplate;
use chaos_traits::catalog::CatalogTool;
use chaos_traits::catalog::McpCatalogSink;

/// Retires callbacks from one connection without retiring its manager's other
/// servers. The lock orders retirement against callbacks already forwarding.
pub(super) struct ClientCatalog {
    live: Mutex<Option<Arc<dyn McpCatalogSink>>>,
}

impl ClientCatalog {
    pub(super) fn new(live: Arc<dyn McpCatalogSink>) -> Self {
        Self {
            live: Mutex::new(Some(live)),
        }
    }

    pub(super) fn retire(&self, server: &str) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(catalog) = live.take() {
            catalog.unregister_mcp(server);
        }
    }

    pub(super) fn replace_tools(&self, server: &str, tools: Vec<CatalogTool>) {
        self.forward(|catalog| {
            catalog.unregister_mcp_tools(server);
            catalog.register_mcp_tools(server, tools);
        });
    }

    fn forward(&self, update: impl FnOnce(&dyn McpCatalogSink)) {
        let live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(catalog) = live.as_ref() {
            update(catalog.as_ref());
        }
    }
}

impl McpCatalogSink for ClientCatalog {
    fn register_mcp_tools(&self, server: &str, tools: Vec<CatalogTool>) {
        self.forward(|catalog| catalog.register_mcp_tools(server, tools));
    }

    fn register_mcp_resources(
        &self,
        server: &str,
        resources: Vec<CatalogResource>,
        templates: Vec<CatalogResourceTemplate>,
    ) {
        self.forward(|catalog| catalog.register_mcp_resources(server, resources, templates));
    }

    fn register_mcp_prompts(&self, server: &str, prompts: Vec<CatalogPrompt>) {
        self.forward(|catalog| catalog.register_mcp_prompts(server, prompts));
    }

    fn unregister_mcp(&self, server: &str) {
        self.forward(|catalog| catalog.unregister_mcp(server));
    }

    fn unregister_mcp_tools(&self, server: &str) {
        self.forward(|catalog| catalog.unregister_mcp_tools(server));
    }

    fn unregister_mcp_resources(&self, server: &str) {
        self.forward(|catalog| catalog.unregister_mcp_resources(server));
    }

    fn unregister_mcp_prompts(&self, server: &str) {
        self.forward(|catalog| catalog.unregister_mcp_prompts(server));
    }

    fn clear_all_mcp(&self) {
        self.forward(|catalog| catalog.clear_all_mcp());
    }
}

#[cfg(test)]
mod tests;
