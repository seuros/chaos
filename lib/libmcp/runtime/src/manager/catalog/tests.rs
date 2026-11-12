use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::*;

#[derive(Default)]
struct RecordingCatalog(AtomicUsize);

impl McpCatalogSink for RecordingCatalog {
    fn register_mcp_tools(&self, _server: &str, _tools: Vec<CatalogTool>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn register_mcp_resources(
        &self,
        _server: &str,
        _resources: Vec<CatalogResource>,
        _templates: Vec<CatalogResourceTemplate>,
    ) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn register_mcp_prompts(&self, _server: &str, _prompts: Vec<CatalogPrompt>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn unregister_mcp(&self, _server: &str) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn unregister_mcp_tools(&self, _server: &str) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn unregister_mcp_resources(&self, _server: &str) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn unregister_mcp_prompts(&self, _server: &str) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn clear_all_mcp(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn retiring_one_connection_blocks_late_callbacks_but_not_other_connections() {
    let live = Arc::new(RecordingCatalog::default());
    let retired = ClientCatalog::new(live.clone());
    let other = ClientCatalog::new(live.clone());
    retired.register_mcp_tools("target", Vec::new());
    retired.retire("target");
    assert_eq!(live.0.load(Ordering::SeqCst), 2);

    retired.register_mcp_tools("target", Vec::new());
    retired.register_mcp_resources("target", Vec::new(), Vec::new());
    retired.register_mcp_prompts("target", Vec::new());
    retired.unregister_mcp("target");
    retired.unregister_mcp_tools("target");
    retired.unregister_mcp_resources("target");
    retired.unregister_mcp_prompts("target");
    retired.clear_all_mcp();
    retired.retire("target");
    assert_eq!(live.0.load(Ordering::SeqCst), 2);

    other.replace_tools("other", Vec::new());
    assert_eq!(live.0.load(Ordering::SeqCst), 4);
}
