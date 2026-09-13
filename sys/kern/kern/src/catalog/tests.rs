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
