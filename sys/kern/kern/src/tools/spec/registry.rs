use std::collections::HashMap;
use std::sync::Arc;

use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::openai_models::ApplyPatchToolType;
use chaos_ipc::openai_models::ConfigShellToolType;
use chaos_ipc::openai_models::WebSearchToolType;
use chaos_mcp_runtime::manager::ToolInfo;

use crate::client_common::tools::ToolSpec;
use crate::tools::registry::ToolRegistryBuilder;

use super::ToolsConfig;
use super::adapters::{annotation_suffix, dynamic_tool_to_model_tool, mcp_tool_to_model_tool};
use super::tool_builders::{
    create_call_mcp_tool_async_tool, create_cancel_mcp_task_tool, create_close_agent_tool,
    create_exec_command_tool, create_list_mcp_resource_templates_tool,
    create_list_mcp_resources_tool, create_read_mcp_resource_tool,
    create_report_agent_job_result_tool, create_request_permissions_tool,
    create_request_user_input_tool, create_resume_agent_tool, create_send_input_tool,
    create_shell_command_tool, create_shell_tool, create_spawn_agent_tool,
    create_spawn_agents_on_csv_tool, create_test_sync_tool, create_view_image_tool,
    create_wait_agent_tool, create_write_stdin_tool,
};

pub(crate) fn push_tool_spec(
    builder: &mut ToolRegistryBuilder,
    spec: ToolSpec,
    supports_parallel_tool_calls: bool,
) {
    tracing::debug!(tool = %spec.name(), "registering tool");
    if supports_parallel_tool_calls {
        builder.push_spec_with_parallel_support(spec, /*supports_parallel_tool_calls*/ true);
    } else {
        builder.push_spec(spec);
    }
}

pub(crate) const WEB_SEARCH_CONTENT_TYPES: [&str; 2] = ["text", "image"];

/// Builds the tool registry builder while collecting tool specs for later
/// serialization. Test-only entry point that discovers catalog tools via
/// inventory.
#[cfg(test)]
pub(crate) fn build_specs(
    config: &ToolsConfig,
    mcp_tools: Option<HashMap<String, chaos_mcp_runtime::manager::McpToolInfo>>,
    app_tools: Option<HashMap<String, ToolInfo>>,
    dynamic_tools: &[DynamicToolSpec],
) -> ToolRegistryBuilder {
    use chaos_traits::catalog::CatalogRegistration;
    use std::collections::HashSet;
    let mut seen_modules = HashSet::new();
    let catalog_tools: Vec<(String, chaos_traits::catalog::CatalogTool)> =
        inventory::iter::<CatalogRegistration>
            .into_iter()
            .filter(|reg| seen_modules.insert(reg.name))
            .flat_map(|reg| {
                let name = reg.name.to_string();
                (reg.tools)().into_iter().map(move |t| (name.clone(), t))
            })
            .collect();
    build_specs_with_discoverable_tools(
        config,
        mcp_tools,
        app_tools,
        dynamic_tools,
        catalog_tools,
        None,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_specs_with_discoverable_tools(
    config: &ToolsConfig,
    mcp_tools: Option<HashMap<String, chaos_mcp_runtime::manager::McpToolInfo>>,
    _app_tools: Option<HashMap<String, ToolInfo>>,
    dynamic_tools: &[DynamicToolSpec],
    catalog_tools: Vec<(String, chaos_traits::catalog::CatalogTool)>,
    hallucinate: Option<chaos_hallucinate::HallucinateHandle>,
    plan_mode: bool,
) -> ToolRegistryBuilder {
    use crate::minions::tools::CloseAgentHandler;
    use crate::minions::tools::ResumeAgentHandler;
    use crate::minions::tools::SendInputHandler;
    use crate::minions::tools::SpawnAgentHandler;
    use crate::minions::tools::WaitAgentHandler;
    use crate::tools::handlers::ApplyPatchHandler;
    use crate::tools::handlers::CatalogModuleHandler;
    use crate::tools::handlers::DynamicToolHandler;
    use crate::tools::handlers::HallucinateHandler;
    use crate::tools::handlers::McpHandler;
    use crate::tools::handlers::McpResourceHandler;
    use crate::tools::handlers::McpTaskHandler;
    use crate::tools::handlers::PLAN_TOOL;
    use crate::tools::handlers::PlanHandler;
    use crate::tools::handlers::RequestPermissionsHandler;
    use crate::tools::handlers::RequestUserInputHandler;
    use crate::tools::handlers::ShellCommandHandler;
    use crate::tools::handlers::ShellHandler;
    use crate::tools::handlers::TestSyncHandler;
    use crate::tools::handlers::UnifiedExecHandler;
    use crate::tools::handlers::ViewImageHandler;
    use chaos_parrot::sanitize::parse_tool_input_schema;
    use chaos_traits::catalog::CatalogRegistration;

    let mut builder = ToolRegistryBuilder::new();

    let shell_handler = Arc::new(ShellHandler);
    let unified_exec_handler = Arc::new(UnifiedExecHandler);
    let plan_handler = Arc::new(PlanHandler);
    let apply_patch_handler = Arc::new(ApplyPatchHandler);
    let dynamic_tool_handler = Arc::new(DynamicToolHandler);
    let view_image_handler = Arc::new(ViewImageHandler);
    let mcp_handler = Arc::new(McpHandler);
    let mcp_resource_handler = Arc::new(McpResourceHandler);
    let mcp_task_handler = Arc::new(McpTaskHandler);
    let shell_command_handler = Arc::new(ShellCommandHandler::new());
    let request_permissions_handler = Arc::new(RequestPermissionsHandler);
    let request_user_input_handler = Arc::new(RequestUserInputHandler {
        default_mode_request_user_input: config.default_mode_request_user_input,
    });
    let exec_permission_approvals_enabled = config.exec_permission_approvals_enabled;

    match &config.shell_type {
        ConfigShellToolType::Default => {
            push_tool_spec(
                &mut builder,
                create_shell_tool(exec_permission_approvals_enabled),
                /*supports_parallel_tool_calls*/ true,
            );
        }
        ConfigShellToolType::Local => {
            push_tool_spec(
                &mut builder,
                ToolSpec::LocalShell {},
                /*supports_parallel_tool_calls*/ true,
            );
        }
        ConfigShellToolType::UnifiedExec => {
            push_tool_spec(
                &mut builder,
                create_exec_command_tool(
                    config.allow_login_shell,
                    exec_permission_approvals_enabled,
                ),
                /*supports_parallel_tool_calls*/ true,
            );
            push_tool_spec(
                &mut builder,
                create_write_stdin_tool(),
                /*supports_parallel_tool_calls*/ false,
            );
            builder.register_handler("exec_command", unified_exec_handler.clone());
            builder.register_handler("write_stdin", unified_exec_handler);
        }
        ConfigShellToolType::Disabled => {
            // Do nothing.
        }
        ConfigShellToolType::ShellCommand => {
            push_tool_spec(
                &mut builder,
                create_shell_command_tool(
                    config.allow_login_shell,
                    exec_permission_approvals_enabled,
                ),
                /*supports_parallel_tool_calls*/ true,
            );
        }
    }

    if config.shell_type != ConfigShellToolType::Disabled {
        builder.register_handler("shell", shell_handler.clone());
        builder.register_handler("container.exec", shell_handler.clone());
        builder.register_handler("local_shell", shell_handler);
        builder.register_handler("shell_command", shell_command_handler);
    }

    if mcp_tools.is_some() {
        push_tool_spec(
            &mut builder,
            create_list_mcp_resources_tool(),
            /*supports_parallel_tool_calls*/ true,
        );
        push_tool_spec(
            &mut builder,
            create_list_mcp_resource_templates_tool(),
            /*supports_parallel_tool_calls*/ true,
        );
        push_tool_spec(
            &mut builder,
            create_read_mcp_resource_tool(),
            /*supports_parallel_tool_calls*/ true,
        );
        builder.register_handler("list_mcp_resources", mcp_resource_handler.clone());
        builder.register_handler("list_mcp_resource_templates", mcp_resource_handler.clone());
        builder.register_handler("read_mcp_resource", mcp_resource_handler);

        push_tool_spec(
            &mut builder,
            create_call_mcp_tool_async_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        push_tool_spec(
            &mut builder,
            create_cancel_mcp_task_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        builder.register_handler("call_mcp_tool_async", mcp_task_handler.clone());
        builder.register_handler("cancel_mcp_task", mcp_task_handler);
    }

    push_tool_spec(
        &mut builder,
        PLAN_TOOL.clone(),
        /*supports_parallel_tool_calls*/ false,
    );
    builder.register_handler("update_plan", plan_handler);

    if config.request_user_input {
        use crate::collaboration_modes::CollaborationModesConfig;
        push_tool_spec(
            &mut builder,
            create_request_user_input_tool(CollaborationModesConfig {
                default_mode_request_user_input: config.default_mode_request_user_input,
            }),
            /*supports_parallel_tool_calls*/ false,
        );
        builder.register_handler("request_user_input", request_user_input_handler);
    }

    if config.request_permissions_tool_enabled {
        push_tool_spec(
            &mut builder,
            create_request_permissions_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        builder.register_handler("request_permissions", request_permissions_handler);
    }

    if let Some(apply_patch_tool_type) = &config.apply_patch_tool_type {
        use crate::tools::handlers::apply_patch::create_apply_patch_freeform_tool;
        use crate::tools::handlers::apply_patch::create_apply_patch_json_tool;
        match apply_patch_tool_type {
            ApplyPatchToolType::Freeform => {
                push_tool_spec(
                    &mut builder,
                    create_apply_patch_freeform_tool(),
                    /*supports_parallel_tool_calls*/ false,
                );
            }
            ApplyPatchToolType::Function => {
                push_tool_spec(
                    &mut builder,
                    create_apply_patch_json_tool(),
                    /*supports_parallel_tool_calls*/ false,
                );
            }
        }
        builder.register_handler("apply_patch", apply_patch_handler);
    }

    // Catalog tools registered by modules via inventory.
    {
        let catalog_registrations: HashMap<&'static str, &'static CatalogRegistration> =
            inventory::iter::<CatalogRegistration>
                .into_iter()
                .map(|reg| (reg.name, reg))
                .collect();
        let mut catalog_tools = catalog_tools;
        catalog_tools.sort_by(|(sa, _), (sb, _)| sa.cmp(sb));
        for (source, tool) in catalog_tools {
            if source == "mcp_task" {
                // These tools are wired explicitly from the `mcp_tools` block above so they only
                // appear when MCP servers are present and are not duplicated via inventory.
                continue;
            }
            let input_schema = parse_tool_input_schema(&tool.input_schema)
                .unwrap_or_else(|e| panic!("catalog tool {} has invalid schema: {e}", tool.name));
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
            let spec = ToolSpec::Function(chaos_parrot::sanitize::ResponsesApiTool {
                name: tool.name.clone(),
                description,
                strict: false,
                defer_loading: None,
                parameters: input_schema,
                output_schema: None,
            });
            let parallel = tool.supports_parallel_tool_calls;
            push_tool_spec(&mut builder, spec, parallel);
            match source.as_str() {
                "hallucinate" => {
                    if let Some(ref handle) = hallucinate {
                        let handler = Arc::new(HallucinateHandler {
                            handle: handle.clone(),
                        });
                        builder.register_handler(&tool.name, handler);
                    }
                }
                _ => {
                    let reg = catalog_registrations.get(source.as_str()).unwrap_or_else(|| {
                        panic!(
                            "catalog module {source} registered tool {} but has no CatalogRegistration",
                            tool.name
                        )
                    });
                    let tool_driver = reg.tool_driver.unwrap_or_else(|| {
                        panic!(
                            "catalog module {source} registered tool {} but provides no tool_driver",
                            tool.name
                        )
                    });
                    let handler = Arc::new(CatalogModuleHandler {
                        driver: tool_driver(),
                        read_only_hint: tool.read_only_hint,
                    });
                    builder.register_handler(&tool.name, handler);
                }
            }
        }
    }

    if config
        .experimental_supported_tools
        .contains(&"test_sync_tool".to_string())
    {
        let test_sync_handler = Arc::new(TestSyncHandler);
        push_tool_spec(
            &mut builder,
            create_test_sync_tool(),
            /*supports_parallel_tool_calls*/ true,
        );
        builder.register_handler("test_sync_tool", test_sync_handler);
    }

    // Skip the chaos-managed web search tool when the provider already injects
    // it as a native server-side tool (e.g. xAI). Sending two `web_search`
    // entries confuses providers that only expect one.
    let native_owns_web_search = config
        .native_server_side_tools
        .iter()
        .any(|t| t == "web_search");

    let external_web_access = if native_owns_web_search {
        None
    } else {
        match config.web_search_mode {
            Some(WebSearchMode::Cached) => Some(false),
            Some(WebSearchMode::Live) => Some(true),
            Some(WebSearchMode::Disabled) | None => None,
        }
    };

    if let Some(external_web_access) = external_web_access {
        let search_content_types = match config.web_search_tool_type {
            WebSearchToolType::Text => None,
            WebSearchToolType::TextAndImage => Some(
                WEB_SEARCH_CONTENT_TYPES
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
        };

        push_tool_spec(
            &mut builder,
            ToolSpec::WebSearch {
                external_web_access: Some(external_web_access),
                filters: config
                    .web_search_config
                    .as_ref()
                    .and_then(|cfg| cfg.filters.clone().map(Into::into)),
                user_location: config
                    .web_search_config
                    .as_ref()
                    .and_then(|cfg| cfg.user_location.clone().map(Into::into)),
                search_context_size: config
                    .web_search_config
                    .as_ref()
                    .and_then(|cfg| cfg.search_context_size),
                search_content_types,
            },
            /*supports_parallel_tool_calls*/ false,
        );
    }

    if config.image_gen_tool {
        push_tool_spec(
            &mut builder,
            ToolSpec::ImageGeneration {
                output_format: "png".to_string(),
            },
            /*supports_parallel_tool_calls*/ false,
        );
    }

    push_tool_spec(
        &mut builder,
        create_view_image_tool(config.can_request_original_image_detail),
        /*supports_parallel_tool_calls*/ true,
    );
    builder.register_handler("view_image", view_image_handler);

    if config.collab_tools {
        push_tool_spec(
            &mut builder,
            create_spawn_agent_tool(config),
            /*supports_parallel_tool_calls*/ false,
        );
        push_tool_spec(
            &mut builder,
            create_send_input_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        push_tool_spec(
            &mut builder,
            create_resume_agent_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        push_tool_spec(
            &mut builder,
            create_wait_agent_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        push_tool_spec(
            &mut builder,
            create_close_agent_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        builder.register_handler("spawn_agent", Arc::new(SpawnAgentHandler));
        builder.register_handler("send_input", Arc::new(SendInputHandler));
        builder.register_handler("resume_agent", Arc::new(ResumeAgentHandler));
        builder.register_handler("wait_agent", Arc::new(WaitAgentHandler));
        builder.register_handler("close_agent", Arc::new(CloseAgentHandler));
    }

    if config.agent_jobs_tools {
        use crate::tools::handlers::agent_jobs::BatchJobHandler;
        let agent_jobs_handler = Arc::new(BatchJobHandler);
        push_tool_spec(
            &mut builder,
            create_spawn_agents_on_csv_tool(),
            /*supports_parallel_tool_calls*/ false,
        );
        builder.register_handler("spawn_agents_on_csv", agent_jobs_handler.clone());
        if config.agent_jobs_worker_tools {
            push_tool_spec(
                &mut builder,
                create_report_agent_job_result_tool(),
                /*supports_parallel_tool_calls*/ false,
            );
            builder.register_handler("report_agent_job_result", agent_jobs_handler);
        }
    }

    if let Some(mcp_tools) = mcp_tools {
        let mut entries: Vec<(String, chaos_mcp_runtime::manager::McpToolInfo)> =
            mcp_tools.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, tool) in entries.into_iter() {
            if plan_mode
                && let Some(ref ann) = tool.annotations
                && (ann.destructive_hint == Some(true) || ann.read_only_hint == Some(false))
            {
                tracing::debug!(
                    tool = %name,
                    annotations = ?ann,
                    destructive = ann.destructive_hint,
                    read_only = ann.read_only_hint,
                    "skipping MCP tool in plan mode",
                );
                continue;
            }
            match mcp_tool_to_model_tool(name.clone(), tool.clone()) {
                Ok(converted_tool) => {
                    tracing::debug!(
                        tool = %name,
                        annotations = ?tool.annotations,
                        "registering MCP tool",
                    );
                    push_tool_spec(
                        &mut builder,
                        ToolSpec::Function(converted_tool),
                        /*supports_parallel_tool_calls*/ false,
                    );
                    builder.register_handler(name, mcp_handler.clone());
                }
                Err(e) => {
                    tracing::error!("Failed to convert {name:?} MCP tool to model tool: {e:?}");
                }
            }
        }
    }

    if !dynamic_tools.is_empty() {
        for tool in dynamic_tools {
            match dynamic_tool_to_model_tool(tool) {
                Ok(converted_tool) => {
                    push_tool_spec(
                        &mut builder,
                        ToolSpec::Function(converted_tool),
                        /*supports_parallel_tool_calls*/ false,
                    );
                    builder.register_handler(tool.name.clone(), dynamic_tool_handler.clone());
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to convert dynamic tool {:?} to model tool: {e:?}",
                        tool.name
                    );
                }
            }
        }
    }

    builder
}
