use crate::client_common::tools::FreeformTool;
use crate::config::test_config;
use crate::models_manager::manager::ModelsManager;
use crate::models_manager::model_info::with_config_overrides;
use crate::tools::ToolRouter;
use crate::tools::registry::ConfiguredToolSpec;
use crate::tools::router::ToolRouterParams;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::openai_models::ModelInfo;
use chaos_parrot::sanitize::AdditionalProperties;
use chaos_parrot::sanitize::JsonSchema;
use chaos_parrot::sanitize::ResponsesApiTool;
use chaos_parrot::sanitize::mcp_call_tool_result_output_schema;
use chaos_parrot::sanitize::parse_tool_input_schema;
use pretty_assertions::assert_eq;

use super::*;

fn mcp_tool(
    name: &str,
    description: &str,
    input_schema: serde_json::Value,
) -> chaos_mcp_runtime::manager::McpToolInfo {
    chaos_mcp_runtime::manager::McpToolInfo {
        name: name.to_string(),
        title: None,
        description: Some(description.to_string()),
        input_schema,
        output_schema: None,
        annotations: None,
        execution: None,
        icons: None,
        meta: None,
    }
}

#[test]
fn mcp_tool_to_openai_tool_inserts_empty_properties() {
    let tool = chaos_mcp_runtime::manager::McpToolInfo {
        name: "no_props".to_string(),
        title: None,
        description: Some("No properties".to_string()),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: None,
        annotations: None,
        execution: None,
        icons: None,
        meta: None,
    };

    let openai_tool =
        mcp_tool_to_openai_tool("server/no_props".to_string(), tool).expect("convert tool");
    let parameters = serde_json::to_value(openai_tool.parameters).expect("serialize schema");

    assert_eq!(parameters.get("properties"), Some(&serde_json::json!({})));
}

#[test]
fn mcp_tool_to_openai_tool_preserves_top_level_output_schema() {
    let tool = chaos_mcp_runtime::manager::McpToolInfo {
        name: "with_output".to_string(),
        title: None,
        description: Some("Has output schema".to_string()),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: Some(serde_json::json!({
            "properties": {
                "result": {
                    "properties": {
                        "nested": {}
                    }
                }
            },
            "required": ["result"]
        })),
        annotations: None,
        execution: None,
        icons: None,
        meta: None,
    };

    let openai_tool = mcp_tool_to_openai_tool("mcp__server__with_output".to_string(), tool)
        .expect("convert tool");

    assert_eq!(
        openai_tool.output_schema,
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "array",
                    "items": {}
                },
                "structuredContent": {
                    "properties": {
                        "result": {
                            "properties": {
                                "nested": {}
                            }
                        }
                    },
                    "required": ["result"]
                },
                "isError": {
                    "type": "boolean"
                },
                "_meta": {}
            },
            "required": ["content"],
            "additionalProperties": false
        }))
    );
}

#[test]
fn mcp_tool_to_openai_tool_preserves_output_schema_without_inferred_type() {
    let tool = chaos_mcp_runtime::manager::McpToolInfo {
        name: "with_enum_output".to_string(),
        title: None,
        description: Some("Has enum output schema".to_string()),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: Some(serde_json::json!({"enum": ["ok", "error"]})),
        annotations: None,
        execution: None,
        icons: None,
        meta: None,
    };

    let openai_tool = mcp_tool_to_openai_tool("mcp__server__with_enum_output".to_string(), tool)
        .expect("convert tool");

    assert_eq!(
        openai_tool.output_schema,
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "array",
                    "items": {}
                },
                "structuredContent": {
                    "enum": ["ok", "error"]
                },
                "isError": {
                    "type": "boolean"
                },
                "_meta": {}
            },
            "required": ["content"],
            "additionalProperties": false
        }))
    );
}

#[test]
fn search_tool_deferred_tools_always_set_defer_loading_true() {
    let tool = mcp_tool(
        "lookup_order",
        "Look up an order",
        serde_json::json!({
            "type": "object",
            "properties": {
                "order_id": {"type": "string"}
            },
            "required": ["order_id"],
            "additionalProperties": false,
        }),
    );

    let openai_tool =
        mcp_tool_to_deferred_openai_tool("mcp__codex_apps__lookup_order".to_string(), tool)
            .expect("convert deferred tool");

    assert_eq!(openai_tool.defer_loading, Some(true));
}

#[test]
fn deferred_responses_api_tool_serializes_with_defer_loading() {
    let tool = mcp_tool(
        "lookup_order",
        "Look up an order",
        serde_json::json!({
            "type": "object",
            "properties": {
                "order_id": {"type": "string"}
            },
            "required": ["order_id"],
            "additionalProperties": false,
        }),
    );

    let serialized = serde_json::to_value(ToolSpec::Function(
        mcp_tool_to_deferred_openai_tool("mcp__codex_apps__lookup_order".to_string(), tool)
            .expect("convert deferred tool"),
    ))
    .expect("serialize deferred tool");

    assert_eq!(
        serialized,
        serde_json::json!({
            "type": "function",
            "name": "mcp__codex_apps__lookup_order",
            "description": "Look up an order",
            "strict": false,
            "defer_loading": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "order_id": {"type": "string"}
                },
                "required": ["order_id"],
                "additionalProperties": false,
            }
        })
    );
}

#[test]
fn dynamic_tool_to_openai_tool_keeps_output_schema_absent() {
    let tool = DynamicToolSpec {
        name: "lookup_ticket".to_string(),
        description: "Fetch a ticket".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"}
            },
            "required": ["id"]
        }),
        defer_loading: false,
    };

    let openai_tool = dynamic_tool_to_openai_tool(&tool).expect("convert dynamic tool");

    assert_eq!(openai_tool.output_schema, None);
    assert_eq!(openai_tool.defer_loading, None);
    assert_eq!(
        openai_tool.parameters,
        JsonSchema::Object {
            properties: BTreeMap::from([(
                "id".to_string(),
                JsonSchema::String { description: None }
            )]),
            required: Some(vec!["id".to_string()]),
            additional_properties: None,
        }
    );
}

fn tool_name(tool: &ToolSpec) -> &str {
    match tool {
        ToolSpec::Function(ResponsesApiTool { name, .. }) => name,
        ToolSpec::ToolSearch { .. } => "tool_search",
        ToolSpec::LocalShell {} => "local_shell",
        ToolSpec::ImageGeneration { .. } => "image_generation",
        ToolSpec::WebSearch { .. } => "web_search",
        ToolSpec::Freeform(FreeformTool { name, .. }) => name,
    }
}

// Avoid order-based assertions; compare via set containment instead.
fn assert_contains_tool_names(tools: &[ConfiguredToolSpec], expected_subset: &[&str]) {
    use std::collections::HashSet;
    let mut names = HashSet::new();
    let mut duplicates = Vec::new();
    for name in tools.iter().map(|t| tool_name(&t.spec)) {
        if !names.insert(name) {
            duplicates.push(name);
        }
    }
    assert!(
        duplicates.is_empty(),
        "duplicate tool entries detected: {duplicates:?}"
    );
    for expected in expected_subset {
        assert!(
            names.contains(expected),
            "expected tool {expected} to be present; had: {names:?}"
        );
    }
}

fn assert_lacks_tool_name(tools: &[ConfiguredToolSpec], expected_absent: &str) {
    let names = tools
        .iter()
        .map(|tool| tool_name(&tool.spec))
        .collect::<Vec<_>>();
    assert!(
        !names.contains(&expected_absent),
        "expected tool {expected_absent} to be absent; had: {names:?}"
    );
}

fn shell_tool_name(config: &ToolsConfig) -> Option<&'static str> {
    match config.shell_type {
        ConfigShellToolType::Default => Some("shell"),
        ConfigShellToolType::Local => Some("local_shell"),
        ConfigShellToolType::UnifiedExec => None,
        ConfigShellToolType::Disabled => None,
        ConfigShellToolType::ShellCommand => Some("shell_command"),
    }
}

fn find_tool<'a>(tools: &'a [ConfiguredToolSpec], expected_name: &str) -> &'a ConfiguredToolSpec {
    tools
        .iter()
        .find(|tool| tool_name(&tool.spec) == expected_name)
        .unwrap_or_else(|| panic!("expected tool {expected_name}"))
}

fn strip_descriptions_schema(schema: &mut JsonSchema) {
    match schema {
        JsonSchema::Boolean { description }
        | JsonSchema::String { description }
        | JsonSchema::Number { description } => {
            *description = None;
        }
        JsonSchema::Array { items, description } => {
            strip_descriptions_schema(items);
            *description = None;
        }
        JsonSchema::Object {
            properties,
            required: _,
            additional_properties,
        } => {
            for v in properties.values_mut() {
                strip_descriptions_schema(v);
            }
            if let Some(AdditionalProperties::Schema(s)) = additional_properties {
                strip_descriptions_schema(s);
            }
        }
    }
}

fn strip_descriptions_tool(spec: &mut ToolSpec) {
    match spec {
        ToolSpec::ToolSearch { parameters, .. } => strip_descriptions_schema(parameters),
        ToolSpec::Function(ResponsesApiTool { parameters, .. }) => {
            strip_descriptions_schema(parameters);
        }
        ToolSpec::Freeform(_)
        | ToolSpec::LocalShell {}
        | ToolSpec::ImageGeneration { .. }
        | ToolSpec::WebSearch { .. } => {}
    }
}

/// Build a [`ModelInfo`] for the given slug with model-specific tool
/// configuration that used to live in the bundled `models.json` catalog.
fn model_info_from_models_json(slug: &str) -> ModelInfo {
    use chaos_ipc::openai_models::ApplyPatchToolType;

    let config = test_config();
    let mut model = crate::test_support::test_model_info(slug);

    // Per-model tool configuration (mirrors the old catalog entries).
    if slug == "gpt-5.1" || slug.contains("codex") || slug.contains("codex") {
        model.shell_type = ConfigShellToolType::ShellCommand;
        model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    } else {
        // Non-Chaos models use the default shell config and do not expose apply_patch.
        model.shell_type = ConfigShellToolType::Default;
        model.apply_patch_tool_type = None;
    }

    with_config_overrides(model, &config)
}

#[test]
fn test_full_toolset_specs_for_gpt5_codex_unified_exec_web_search() {
    let model_info = model_info_from_models_json("gpt-5-codex");
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&config, None, None, &[]).build();

    // Build actual map name -> spec
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    let mut actual: BTreeMap<String, ToolSpec> = BTreeMap::from([]);
    let mut duplicate_names = Vec::new();
    for t in &tools {
        let name = tool_name(&t.spec).to_string();
        if actual.insert(name.clone(), t.spec.clone()).is_some() {
            duplicate_names.push(name);
        }
    }
    assert!(
        duplicate_names.is_empty(),
        "duplicate tool entries detected: {duplicate_names:?}"
    );

    // Build expected from the same helpers used by the builder.
    let mut expected: BTreeMap<String, ToolSpec> = BTreeMap::from([]);
    for spec in [
        create_exec_command_tool(true, false),
        create_write_stdin_tool(),
        PLAN_TOOL.clone(),
        create_request_user_input_tool(CollaborationModesConfig::default()),
        create_apply_patch_freeform_tool(),
        ToolSpec::WebSearch {
            external_web_access: Some(true),
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: None,
        },
        create_view_image_tool(config.can_request_original_image_detail),
        create_spawn_agent_tool(&config),
        create_send_input_tool(),
        create_resume_agent_tool(),
        create_wait_agent_tool(),
        create_close_agent_tool(),
    ] {
        expected.insert(tool_name(&spec).to_string(), spec);
    }

    // Catalog tools (arsenal, cron, git, mcp, etc.) — collected from inventory,
    // matching the same path the builder uses so annotations are consistent.
    {
        use chaos_traits::catalog::CatalogRegistration;
        let mut seen = std::collections::HashSet::new();
        for reg in inventory::iter::<CatalogRegistration> {
            if !seen.insert(reg.name) {
                continue;
            }
            for tool in (reg.tools)() {
                let input_schema =
                    parse_tool_input_schema(&tool.input_schema).unwrap_or_else(|e| {
                        panic!("catalog tool {} has invalid schema: {e}", tool.name)
                    });
                let description = match tool.annotations.as_ref().and_then(|v| {
                    serde_json::from_value::<chaos_mcp_runtime::ToolAnnotations>(v.clone()).ok()
                }) {
                    Some(ann) => {
                        let suffix = annotation_suffix(&ann);
                        if suffix.is_empty() {
                            tool.description
                        } else {
                            format!("{}{suffix}", tool.description)
                        }
                    }
                    None => tool.description,
                };
                let spec = ToolSpec::Function(ResponsesApiTool {
                    name: tool.name.clone(),
                    description,
                    strict: false,
                    defer_loading: None,
                    parameters: input_schema,
                    output_schema: None,
                });
                expected.insert(tool_name(&spec).to_string(), spec);
            }
        }
    }

    if config.exec_permission_approvals_enabled {
        let spec = create_request_permissions_tool();
        expected.insert(tool_name(&spec).to_string(), spec);
    }

    // Exact name set match — this is the only test allowed to fail when tools change.
    let actual_names: HashSet<_> = actual.keys().cloned().collect();
    let expected_names: HashSet<_> = expected.keys().cloned().collect();
    assert_eq!(actual_names, expected_names, "tool name set mismatch");

    // Compare specs ignoring human-readable descriptions.
    for name in expected.keys() {
        let mut a = actual.get(name).expect("present").clone();
        let mut e = expected.get(name).expect("present").clone();
        strip_descriptions_tool(&mut a);
        strip_descriptions_tool(&mut e);
        assert_eq!(a, e, "spec mismatch for {name}");
    }
}

#[test]
fn arsenal_tools_keep_closed_object_schemas() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    for name in ["read_file", "grep_files", "list_dir"] {
        let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = &find_tool(&tools, name).spec
        else {
            panic!("expected function tool");
        };
        let JsonSchema::Object {
            additional_properties,
            ..
        } = parameters
        else {
            panic!("{name} should use an object schema");
        };
        assert_eq!(
            additional_properties,
            &Some(false.into()),
            "{name} should reject unknown arguments"
        );
    }
}

#[test]
fn arsenal_read_file_preserves_indentation_object_schema() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) =
        &find_tool(&tools, "read_file").spec
    else {
        panic!("expected function tool");
    };
    let JsonSchema::Object { properties, .. } = parameters else {
        panic!("read_file should use an object schema");
    };

    let Some(JsonSchema::Object {
        properties: indentation_properties,
        additional_properties,
        ..
    }) = properties.get("indentation")
    else {
        panic!("indentation should remain an object schema");
    };

    assert_eq!(
        additional_properties,
        &Some(false.into()),
        "indentation should reject unknown keys"
    );
    assert!(indentation_properties.contains_key("anchor_line"));
    assert!(indentation_properties.contains_key("max_levels"));
    assert!(indentation_properties.contains_key("include_siblings"));
}

#[test]
fn test_build_specs_collab_tools_enabled() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();
    assert_contains_tool_names(
        &tools,
        &["spawn_agent", "send_input", "wait_agent", "close_agent"],
    );
    assert_lacks_tool_name(&tools, "spawn_agents_on_csv");
}

#[test]
fn test_build_specs_enable_fanout_enables_agent_jobs_and_collab_tools() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::SpawnCsv);

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();
    assert_contains_tool_names(
        &tools,
        &[
            "spawn_agent",
            "send_input",
            "wait_agent",
            "close_agent",
            "spawn_agents_on_csv",
        ],
    );
}

#[test]
fn view_image_tool_includes_detail_with_original_detail_feature() {
    let config = test_config();
    let mut model_info =
        ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    model_info.supports_image_detail_original = true;
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();
    let view_image = find_tool(&tools, VIEW_IMAGE_TOOL_NAME);
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = &view_image.spec else {
        panic!("view_image should be a function tool");
    };
    let JsonSchema::Object { properties, .. } = parameters else {
        panic!("view_image should use an object schema");
    };
    assert!(properties.contains_key("detail"));
    let Some(JsonSchema::String {
        description: Some(description),
    }) = properties.get("detail")
    else {
        panic!("view_image detail should include a description");
    };
    assert!(description.contains("only supported value is `original`"));
    assert!(description.contains("omit this field for default resized behavior"));
}

#[test]
fn test_build_specs_agent_job_worker_tools_enabled() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::SpawnCsv);

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::SubAgent(SubAgentSource::Other(
            "agent_job:test".to_string(),
        )),
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();
    assert_contains_tool_names(
        &tools,
        &[
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
            "spawn_agents_on_csv",
            "report_agent_job_result",
        ],
    );
    assert_lacks_tool_name(&tools, "request_user_input");
}

#[test]
fn request_user_input_description_reflects_default_mode_feature_flag() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();
    let request_user_input_tool = find_tool(&tools, "request_user_input");
    assert_eq!(
        request_user_input_tool.spec,
        create_request_user_input_tool(CollaborationModesConfig {
            default_mode_request_user_input: true,
        })
    );
}

#[test]
fn request_permissions_requires_feature_flag() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();
    assert_lacks_tool_name(&tools, "request_permissions");

    let mut features = Features::with_defaults();
    features.enable(Feature::RequestPermissionsTool);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();
    let request_permissions_tool = find_tool(&tools, "request_permissions");
    assert_eq!(
        request_permissions_tool.spec,
        create_request_permissions_tool()
    );
}

#[test]
fn request_permissions_tool_is_independent_from_additional_permissions() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::ExecPermissionApprovals);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    assert_lacks_tool_name(&tools, "request_permissions");
}

fn assert_model_tools(
    model_slug: &str,
    features: &Features,
    web_search_mode: Option<WebSearchMode>,
    expected_tools: &[&str],
) {
    let _config = test_config();
    let model_info = model_info_from_models_json(model_slug);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features,
        web_search_mode,
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let router = ToolRouter::from_config(
        &tools_config,
        ToolRouterParams {
            mcp_tools: None,
            app_tools: None,
            dynamic_tools: &[],
            catalog_tools: {
                use chaos_traits::catalog::CatalogRegistration;
                use std::collections::HashSet;
                let mut seen = HashSet::new();
                inventory::iter::<CatalogRegistration>
                    .into_iter()
                    .filter(|reg| seen.insert(reg.name))
                    .flat_map(|reg| {
                        let name = reg.name.to_string();
                        (reg.tools)().into_iter().map(move |t| (name.clone(), t))
                    })
                    .collect()
            },
            hallucinate: None,
            plan_mode: false,
        },
    );
    let model_visible_specs = router.model_visible_specs();
    let tool_names = model_visible_specs
        .iter()
        .map(ToolSpec::name)
        .collect::<Vec<_>>();
    assert_eq!(
        &tool_names, &expected_tools,
        "model_slug={model_slug}, web_search_mode={web_search_mode:?}"
    );
}

fn assert_default_model_tools(
    model_slug: &str,
    features: &Features,
    web_search_mode: Option<WebSearchMode>,
    shell_tool: &'static str,
    expected_tail: &[&str],
) {
    let _ = shell_tool;
    let mut expected = vec!["exec_command", "write_stdin"];
    expected.extend(expected_tail);
    assert_model_tools(model_slug, features, web_search_mode, &expected);
}

const GPT_5_DEFAULT_TOOL_TAIL: &[&str] = &[
    "update_plan",
    "request_user_input",
    "read_file",
    "grep_files",
    "list_dir",
    "cron_create",
    "cron_toggle",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_repo",
    "git_status",
    "git_branches",
    "git_remotes",
    "mcp_add_server",
    "mcp_server",
    "web_search",
    "view_image",
    "spawn_agent",
    "send_input",
    "resume_agent",
    "wait_agent",
    "close_agent",
];

const GPT_5_1_TOOL_TAIL: &[&str] = &[
    "update_plan",
    "request_user_input",
    "apply_patch",
    "read_file",
    "grep_files",
    "list_dir",
    "cron_create",
    "cron_toggle",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_repo",
    "git_status",
    "git_branches",
    "git_remotes",
    "mcp_add_server",
    "mcp_server",
    "web_search",
    "view_image",
    "spawn_agent",
    "send_input",
    "resume_agent",
    "wait_agent",
    "close_agent",
];

#[derive(Clone, Copy)]
struct DefaultModelToolCase {
    model_slug: &'static str,
    shell_tool: &'static str,
    expected_tail: &'static [&'static str],
}

fn assert_default_model_tool_cases(cases: &[DefaultModelToolCase]) {
    let features = Features::with_defaults();

    for case in cases {
        assert_default_model_tools(
            case.model_slug,
            &features,
            Some(WebSearchMode::Cached),
            case.shell_tool,
            case.expected_tail,
        );
    }
}

#[derive(Clone, Copy)]
struct UnifiedExecWebSearchModelToolCase {
    model_slug: &'static str,
    expected_tail: &'static [&'static str],
}

fn assert_unified_exec_web_search_model_tool_cases(cases: &[UnifiedExecWebSearchModelToolCase]) {
    let features = Features::with_defaults();

    for case in cases {
        let mut expected = vec!["exec_command", "write_stdin"];
        expected.extend_from_slice(case.expected_tail);
        assert_model_tools(
            case.model_slug,
            &features,
            Some(WebSearchMode::Live),
            &expected,
        );
    }
}

#[test]
fn web_search_mode_cached_sets_external_web_access_false() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    let tool = find_tool(&tools, "web_search");
    assert_eq!(
        tool.spec,
        ToolSpec::WebSearch {
            external_web_access: Some(false),
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: None,
        }
    );
}

#[test]
fn web_search_mode_live_sets_external_web_access_true() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    let tool = find_tool(&tools, "web_search");
    assert_eq!(
        tool.spec,
        ToolSpec::WebSearch {
            external_web_access: Some(true),
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: None,
        }
    );
}

#[test]
fn web_search_config_is_forwarded_to_tool_spec() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let web_search_config = WebSearchConfig {
        filters: Some(chaos_ipc::config_types::WebSearchFilters {
            allowed_domains: Some(vec!["example.com".to_string()]),
        }),
        user_location: Some(chaos_ipc::config_types::WebSearchUserLocation {
            r#type: chaos_ipc::config_types::WebSearchUserLocationType::Approximate,
            country: Some("US".to_string()),
            region: Some("California".to_string()),
            city: Some("San Francisco".to_string()),
            timezone: Some("America/Los_Angeles".to_string()),
        }),
        search_context_size: Some(chaos_ipc::config_types::WebSearchContextSize::High),
    };

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    })
    .with_web_search_config(Some(web_search_config.clone()));
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    let tool = find_tool(&tools, "web_search");
    assert_eq!(
        tool.spec,
        ToolSpec::WebSearch {
            external_web_access: Some(true),
            filters: web_search_config
                .filters
                .map(crate::client_common::tools::ResponsesApiWebSearchFilters::from),
            user_location: web_search_config
                .user_location
                .map(crate::client_common::tools::ResponsesApiWebSearchUserLocation::from),
            search_context_size: web_search_config.search_context_size,
            search_content_types: None,
        }
    );
}

#[test]
fn web_search_tool_type_text_and_image_sets_search_content_types() {
    let config = test_config();
    let mut model_info =
        ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    model_info.web_search_tool_type = WebSearchToolType::TextAndImage;
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    let tool = find_tool(&tools, "web_search");
    assert_eq!(
        tool.spec,
        ToolSpec::WebSearch {
            external_web_access: Some(true),
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: Some(
                WEB_SEARCH_CONTENT_TYPES
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            ),
        }
    );
}

#[test]
fn mcp_resource_tools_are_hidden_without_mcp_servers() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    assert!(
        !tools.iter().any(|tool| matches!(
            tool.spec.name(),
            "list_mcp_resources" | "list_mcp_resource_templates" | "read_mcp_resource"
        )),
        "MCP resource tools should be omitted when no MCP servers are configured"
    );
}

#[test]
fn mcp_resource_tools_are_included_when_mcp_servers_are_present() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, Some(HashMap::new()), None, &[]).build();

    assert_contains_tool_names(
        &tools,
        &[
            "list_mcp_resources",
            "list_mcp_resource_templates",
            "read_mcp_resource",
        ],
    );
}

#[test]
fn spawn_agent_tool_description_uses_current_role_names() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });

    let ToolSpec::Function(ResponsesApiTool { description, .. }) =
        create_spawn_agent_tool(&tools_config)
    else {
        panic!("expected function tool");
    };

    assert!(description.contains("task subtasks"));
    assert!(description.contains("scout analysis"));
    assert!(!description.contains("worker subtasks"));
    assert!(!description.contains("explorer analysis"));
}

#[test]
fn test_build_specs_gpt_default_toolsets() {
    assert_default_model_tool_cases(&[
        DefaultModelToolCase {
            model_slug: "gpt-5-codex",
            shell_tool: "shell_command",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
        DefaultModelToolCase {
            model_slug: "gpt-5.1-codex",
            shell_tool: "shell_command",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
        DefaultModelToolCase {
            model_slug: "gpt-5.1-codex-max",
            shell_tool: "shell_command",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
        DefaultModelToolCase {
            model_slug: "gpt-5.1-codex-mini",
            shell_tool: "shell_command",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
        DefaultModelToolCase {
            model_slug: "gpt-5",
            shell_tool: "shell",
            expected_tail: GPT_5_DEFAULT_TOOL_TAIL,
        },
        DefaultModelToolCase {
            model_slug: "gpt-5.1",
            shell_tool: "shell_command",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
    ]);
}

#[test]
fn test_build_specs_gpt_unified_exec_web_search_toolsets() {
    assert_unified_exec_web_search_model_tool_cases(&[
        UnifiedExecWebSearchModelToolCase {
            model_slug: "gpt-5-codex",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
        UnifiedExecWebSearchModelToolCase {
            model_slug: "gpt-5.1-codex",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
        UnifiedExecWebSearchModelToolCase {
            model_slug: "gpt-5.1-codex-max",
            expected_tail: GPT_5_1_TOOL_TAIL,
        },
    ]);
}

#[test]
fn test_build_specs_default_shell_present() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("o3", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, Some(HashMap::new()), None, &[]).build();

    // Only check the shell variant and a couple of core tools.
    let mut subset = vec!["exec_command", "write_stdin", "update_plan"];
    if let Some(shell_tool) = shell_tool_name(&tools_config) {
        subset.push(shell_tool);
    }
    assert_contains_tool_names(&tools, &subset);
}

#[test]
#[ignore]
fn test_parallel_support_flags() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    assert!(find_tool(&tools, "exec_command").supports_parallel_tool_calls);
    assert!(!find_tool(&tools, "write_stdin").supports_parallel_tool_calls);
    assert!(find_tool(&tools, "grep_files").supports_parallel_tool_calls);
    assert!(find_tool(&tools, "list_dir").supports_parallel_tool_calls);
    assert!(find_tool(&tools, "read_file").supports_parallel_tool_calls);
}

#[test]
fn test_test_model_info_includes_sync_tool() {
    let _config = test_config();
    let mut model_info = model_info_from_models_json("gpt-5-codex");
    model_info.experimental_supported_tools = vec![
        "test_sync_tool".to_string(),
        "read_file".to_string(),
        "grep_files".to_string(),
        "list_dir".to_string(),
    ];
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(&tools_config, None, None, &[]).build();

    assert!(
        tools
            .iter()
            .any(|tool| tool_name(&tool.spec) == "test_sync_tool")
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool_name(&tool.spec) == "read_file")
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool_name(&tool.spec) == "grep_files")
    );
    assert!(tools.iter().any(|tool| tool_name(&tool.spec) == "list_dir"));
}

#[test]
fn test_build_specs_mcp_tools_converted() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("o3", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "test_server/do_something_cool".to_string(),
            mcp_tool(
                "do_something_cool",
                "Do something cool",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "string_argument": { "type": "string" },
                        "number_argument": { "type": "number" },
                        "object_argument": {
                            "type": "object",
                            "properties": {
                                "string_property": { "type": "string" },
                                "number_property": { "type": "number" },
                            },
                            "required": ["string_property", "number_property"],
                            "additionalProperties": false,
                        },
                    },
                }),
            ),
        )])),
        None,
        &[],
    )
    .build();

    let tool = find_tool(&tools, "test_server/do_something_cool");
    assert_eq!(
        &tool.spec,
        &ToolSpec::Function(ResponsesApiTool {
            name: "test_server/do_something_cool".to_string(),
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([
                    (
                        "string_argument".to_string(),
                        JsonSchema::String { description: None }
                    ),
                    (
                        "number_argument".to_string(),
                        JsonSchema::Number { description: None }
                    ),
                    (
                        "object_argument".to_string(),
                        JsonSchema::Object {
                            properties: BTreeMap::from([
                                (
                                    "string_property".to_string(),
                                    JsonSchema::String { description: None }
                                ),
                                (
                                    "number_property".to_string(),
                                    JsonSchema::Number { description: None }
                                ),
                            ]),
                            required: Some(vec![
                                "string_property".to_string(),
                                "number_property".to_string(),
                            ]),
                            additional_properties: Some(false.into()),
                        },
                    ),
                ]),
                required: None,
                additional_properties: None,
            },
            description: "Do something cool".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        })
    );
}

#[test]
fn test_build_specs_mcp_tools_sorted_by_name() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("o3", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });

    // Intentionally construct a map with keys that would sort alphabetically.
    let tools_map: HashMap<String, chaos_mcp_runtime::manager::McpToolInfo> = HashMap::from([
        (
            "test_server/do".to_string(),
            mcp_tool("a", "a", serde_json::json!({"type": "object"})),
        ),
        (
            "test_server/something".to_string(),
            mcp_tool("b", "b", serde_json::json!({"type": "object"})),
        ),
        (
            "test_server/cool".to_string(),
            mcp_tool("c", "c", serde_json::json!({"type": "object"})),
        ),
    ]);

    let (tools, _) = build_specs(&tools_config, Some(tools_map), None, &[]).build();

    // Only assert that the MCP tools themselves are sorted by fully-qualified name.
    let mcp_names: Vec<_> = tools
        .iter()
        .map(|t| tool_name(&t.spec).to_string())
        .filter(|n| n.starts_with("test_server/"))
        .collect();
    let expected = vec![
        "test_server/cool".to_string(),
        "test_server/do".to_string(),
        "test_server/something".to_string(),
    ];
    assert_eq!(mcp_names, expected);
}

#[test]
fn test_mcp_tool_property_missing_type_defaults_to_string() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/search".to_string(),
            mcp_tool(
                "search",
                "Search docs",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"description": "search query"}
                    }
                }),
            ),
        )])),
        None,
        &[],
    )
    .build();

    let tool = find_tool(&tools, "dash/search");
    assert_eq!(
        tool.spec,
        ToolSpec::Function(ResponsesApiTool {
            name: "dash/search".to_string(),
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([(
                    "query".to_string(),
                    JsonSchema::String {
                        description: Some("search query".to_string())
                    }
                )]),
                required: None,
                additional_properties: None,
            },
            description: "Search docs".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        })
    );
}

#[test]
fn test_mcp_tool_integer_normalized_to_number() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/paginate".to_string(),
            mcp_tool(
                "paginate",
                "Pagination",
                serde_json::json!({
                    "type": "object",
                    "properties": {"page": {"type": "integer"}}
                }),
            ),
        )])),
        None,
        &[],
    )
    .build();

    let tool = find_tool(&tools, "dash/paginate");
    assert_eq!(
        tool.spec,
        ToolSpec::Function(ResponsesApiTool {
            name: "dash/paginate".to_string(),
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([(
                    "page".to_string(),
                    JsonSchema::Number { description: None }
                )]),
                required: None,
                additional_properties: None,
            },
            description: "Pagination".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        })
    );
}

#[test]
fn test_mcp_tool_array_without_items_gets_default_string_items() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/tags".to_string(),
            mcp_tool(
                "tags",
                "Tags",
                serde_json::json!({
                    "type": "object",
                    "properties": {"tags": {"type": "array"}}
                }),
            ),
        )])),
        None,
        &[],
    )
    .build();

    let tool = find_tool(&tools, "dash/tags");
    assert_eq!(
        tool.spec,
        ToolSpec::Function(ResponsesApiTool {
            name: "dash/tags".to_string(),
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([(
                    "tags".to_string(),
                    JsonSchema::Array {
                        items: Box::new(JsonSchema::String { description: None }),
                        description: None
                    }
                )]),
                required: None,
                additional_properties: None,
            },
            description: "Tags".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        })
    );
}

#[test]
fn test_mcp_tool_anyof_defaults_to_string() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/value".to_string(),
            mcp_tool(
                "value",
                "AnyOf Value",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "value": {"anyOf": [{"type": "string"}, {"type": "number"}]}
                    }
                }),
            ),
        )])),
        None,
        &[],
    )
    .build();

    let tool = find_tool(&tools, "dash/value");
    assert_eq!(
        tool.spec,
        ToolSpec::Function(ResponsesApiTool {
            name: "dash/value".to_string(),
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([(
                    "value".to_string(),
                    JsonSchema::String { description: None }
                )]),
                required: None,
                additional_properties: None,
            },
            description: "AnyOf Value".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        })
    );
}

#[test]
fn test_shell_tool() {
    let tool = super::create_shell_tool(false);
    let ToolSpec::Function(ResponsesApiTool {
        description, name, ..
    }) = &tool
    else {
        panic!("expected function tool");
    };
    assert_eq!(name, "shell");

    let expected = r#"Runs a shell command and returns its output.
- The arguments to `shell` will be passed to execvp(). Most terminal commands should be prefixed with ["bash", "-lc"].
- Always set the `workdir` param when using the shell function. Do not use `cd` unless absolutely necessary."#.to_string();
    assert_eq!(description, &expected);
}

#[test]
fn shell_tool_with_request_permission_includes_additional_permissions() {
    let tool = super::create_shell_tool(true);
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = tool else {
        panic!("expected function tool");
    };
    let JsonSchema::Object { properties, .. } = parameters else {
        panic!("expected object parameters");
    };

    assert!(properties.contains_key("additional_permissions"));

    let Some(JsonSchema::String {
        description: Some(description),
    }) = properties.get("sandbox_permissions")
    else {
        panic!("expected sandbox_permissions description");
    };
    assert!(description.contains("with_additional_permissions"));
    assert!(description.contains("filesystem or network permissions"));

    let Some(JsonSchema::Object {
        properties: additional_properties,
        ..
    }) = properties.get("additional_permissions")
    else {
        panic!("expected additional_permissions schema");
    };
    assert!(additional_properties.contains_key("network"));
    assert!(additional_properties.contains_key("file_system"));
    assert!(!additional_properties.contains_key("macos"));
}

#[test]
fn request_permissions_tool_includes_full_permission_schema() {
    let tool = super::create_request_permissions_tool();
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = tool else {
        panic!("expected function tool");
    };
    let JsonSchema::Object { properties, .. } = parameters else {
        panic!("expected object parameters");
    };
    let Some(JsonSchema::Object {
        properties: permission_properties,
        additional_properties,
        ..
    }) = properties.get("permissions")
    else {
        panic!("expected permissions object");
    };

    assert_eq!(additional_properties, &Some(false.into()));
    assert!(permission_properties.contains_key("network"));
    assert!(permission_properties.contains_key("file_system"));
    assert!(!permission_properties.contains_key("macos"));

    let Some(JsonSchema::Object {
        properties: network_properties,
        additional_properties,
        ..
    }) = permission_properties.get("network")
    else {
        panic!("expected network object");
    };
    assert_eq!(additional_properties, &Some(false.into()));
    assert!(network_properties.contains_key("enabled"));

    let Some(JsonSchema::Object {
        properties: file_system_properties,
        additional_properties,
        ..
    }) = permission_properties.get("file_system")
    else {
        panic!("expected file_system object");
    };
    assert_eq!(additional_properties, &Some(false.into()));
    assert!(file_system_properties.contains_key("read"));
    assert!(file_system_properties.contains_key("write"));
}

#[test]
fn test_shell_command_tool() {
    let tool = super::create_shell_command_tool(true, false);
    let ToolSpec::Function(ResponsesApiTool {
        description, name, ..
    }) = &tool
    else {
        panic!("expected function tool");
    };
    assert_eq!(name, "shell_command");

    let expected = r#"Runs a shell command and returns its output.
- Always set the `workdir` param when using the shell_command function. Do not use `cd` unless absolutely necessary."#.to_string();
    assert_eq!(description, &expected);
}

#[test]
fn test_get_openai_tools_mcp_tools_with_additional_properties_schema() {
    let config = test_config();
    let model_info = ModelsManager::construct_model_info_offline_for_tests("gpt-5-codex", &config);
    let features = Features::with_defaults();

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        sandbox_policy: &SandboxPolicy::RootAccess,
        collab_enabled: true,
    });
    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "test_server/do_something_cool".to_string(),
            mcp_tool(
                "do_something_cool",
                "Do something cool",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "string_argument": {"type": "string"},
                        "number_argument": {"type": "number"},
                        "object_argument": {
                            "type": "object",
                            "properties": {
                                "string_property": {"type": "string"},
                                "number_property": {"type": "number"}
                            },
                            "required": ["string_property", "number_property"],
                            "additionalProperties": {
                                "type": "object",
                                "properties": {
                                    "addtl_prop": {"type": "string"}
                                },
                                "required": ["addtl_prop"],
                                "additionalProperties": false
                            }
                        }
                    }
                }),
            ),
        )])),
        None,
        &[],
    )
    .build();

    let tool = find_tool(&tools, "test_server/do_something_cool");
    assert_eq!(
        tool.spec,
        ToolSpec::Function(ResponsesApiTool {
            name: "test_server/do_something_cool".to_string(),
            parameters: JsonSchema::Object {
                properties: BTreeMap::from([
                    (
                        "string_argument".to_string(),
                        JsonSchema::String { description: None }
                    ),
                    (
                        "number_argument".to_string(),
                        JsonSchema::Number { description: None }
                    ),
                    (
                        "object_argument".to_string(),
                        JsonSchema::Object {
                            properties: BTreeMap::from([
                                (
                                    "string_property".to_string(),
                                    JsonSchema::String { description: None }
                                ),
                                (
                                    "number_property".to_string(),
                                    JsonSchema::Number { description: None }
                                ),
                            ]),
                            required: Some(vec![
                                "string_property".to_string(),
                                "number_property".to_string(),
                            ]),
                            additional_properties: Some(
                                JsonSchema::Object {
                                    properties: BTreeMap::from([(
                                        "addtl_prop".to_string(),
                                        JsonSchema::String { description: None }
                                    ),]),
                                    required: Some(vec!["addtl_prop".to_string(),]),
                                    additional_properties: Some(false.into()),
                                }
                                .into()
                            ),
                        },
                    ),
                ]),
                required: None,
                additional_properties: None,
            },
            description: "Do something cool".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        })
    );
}

#[test]
fn chat_tools_include_top_level_name() {
    let properties =
        BTreeMap::from([("foo".to_string(), JsonSchema::String { description: None })]);
    let tools = vec![ToolSpec::Function(ResponsesApiTool {
        name: "demo".to_string(),
        description: "A demo tool".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: None,
        },
        output_schema: None,
    })];

    let responses_json = create_tools_json_for_responses_api(&tools).unwrap();
    assert_eq!(
        responses_json,
        vec![json!({
            "type": "function",
            "name": "demo",
            "description": "A demo tool",
            "strict": false,
            "parameters": {
                "type": "object",
                "properties": {
                    "foo": { "type": "string" }
                },
            },
        })]
    );
}
