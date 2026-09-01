use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use chaos_ipc::custom_prompts::CustomPrompt;
use chaos_ipc::dynamic_tools::DynamicToolResponse;
use chaos_ipc::protocol::AllToolsResponseEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ListCustomPromptsResponseEvent;
use chaos_ipc::protocol::ToolSummary;

use crate::catalog::Catalog;
use crate::catalog::CatalogSource;
use crate::chaos::Session;
use crate::client_common::tools::ToolSpec;
use crate::config::Config;
use crate::mcp::auth::compute_auth_statuses;
use crate::mcp::collect_mcp_snapshot_from_registry;

fn annotation_labels(tool: &chaos_traits::catalog::CatalogTool) -> Vec<String> {
    tool.annotations
        .as_ref()
        .and_then(|value| {
            serde_json::from_value::<chaos_mcp_runtime::ToolAnnotations>(value.clone()).ok()
        })
        .map(|annotations| {
            let mut labels = crate::tools::spec::annotation_labels(&annotations);
            let has_read_semantics = labels
                .iter()
                .any(|label| label == "read-only" || label == "writes");
            if !has_read_semantics && let Some(read_only) = tool.read_only_hint {
                labels.push(if read_only { "read-only" } else { "writes" }.to_string());
            }
            labels
        })
        .or_else(|| {
            tool.read_only_hint
                .map(|read_only| vec![if read_only { "read-only" } else { "writes" }.to_string()])
        })
        .unwrap_or_default()
}

struct McpToolPresentation {
    server_name: String,
    display_name: String,
    catalog_tool: chaos_traits::catalog::CatalogTool,
}

type McpToolPresentations = HashMap<String, McpToolPresentation>;

struct ToolPresentation {
    display_name: String,
    source: String,
    annotation_labels: Vec<String>,
    annotations: Option<serde_json::Value>,
}

fn tool_presentation(
    name: &str,
    catalog: &Catalog,
    mcp_tools: &McpToolPresentations,
    dynamic_tool_names: &HashSet<String>,
    script_tool_names: &HashSet<String>,
) -> ToolPresentation {
    if name.starts_with("mcp__")
        && let Some(tool) = mcp_tools.get(name)
    {
        return ToolPresentation {
            display_name: tool.display_name.clone(),
            source: format!("mcp:{}", tool.server_name),
            annotation_labels: annotation_labels(&tool.catalog_tool),
            annotations: tool.catalog_tool.annotations.clone(),
        };
    }

    let catalog_entry = catalog.tools().iter().find(|(_, tool)| tool.name == name);
    match catalog_entry {
        Some((source, tool)) => {
            let source = match source {
                CatalogSource::Module(name) => name.clone(),
                CatalogSource::Mcp(name) => format!("mcp:{name}"),
            };
            ToolPresentation {
                display_name: name.to_string(),
                source,
                annotation_labels: annotation_labels(tool),
                annotations: tool.annotations.clone(),
            }
        }
        None if dynamic_tool_names.contains(name) => ToolPresentation {
            display_name: name.to_string(),
            source: "dynamic".to_string(),
            annotation_labels: Vec::new(),
            annotations: None,
        },
        None if script_tool_names.contains(name) => ToolPresentation {
            display_name: name.to_string(),
            source: "halluacinate".to_string(),
            annotation_labels: Vec::new(),
            annotations: None,
        },
        None => ToolPresentation {
            display_name: name.to_string(),
            source: "builtin".to_string(),
            annotation_labels: Vec::new(),
            annotations: None,
        },
    }
}

fn summarize_model_visible_tools(
    specs: &[ToolSpec],
    catalog: &Catalog,
    mcp_tools: &McpToolPresentations,
    dynamic_tool_names: &HashSet<String>,
    script_tool_names: &HashSet<String>,
) -> Vec<ToolSummary> {
    specs
        .iter()
        .map(|spec| {
            let name = spec.name();
            let presentation = tool_presentation(
                name,
                catalog,
                mcp_tools,
                dynamic_tool_names,
                script_tool_names,
            );
            ToolSummary {
                name: presentation.display_name,
                description: spec.description().to_string(),
                annotation_labels: presentation.annotation_labels,
                annotations: presentation.annotations,
                source: presentation.source,
            }
        })
        .collect()
}

pub async fn list_all_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
    let (turn_context, cancellation_token) = sess.tool_listing_turn_context(sub_id.clone()).await;
    let router = match crate::chaos::turn::built_tools(
        sess,
        &turn_context,
        &[],
        &cancellation_token,
    )
    .await
    {
        Ok(router) => router,
        Err(err) => {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(crate::protocol::ErrorEvent {
                    message: format!("Failed to list model-visible tools: {err}"),
                    chaos_error_info: None,
                }),
            })
            .await;
            return;
        }
    };

    let dynamic_tool_names = turn_context
        .dynamic_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let script_tool_names = if let Some(ref handle) = sess.services.halluacinate {
        handle
            .list_tools()
            .await
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    } else {
        HashSet::new()
    };
    let mcp_tools = sess
        .services
        .mcp_registry
        .current_manager()
        .list_all_tools()
        .await
        .into_iter()
        .map(|(qualified_name, tool)| {
            let catalog_tool =
                chaos_mcp_runtime::catalog_conv::mcp_tool_info_to_catalog_tool(&tool);
            (
                qualified_name,
                McpToolPresentation {
                    server_name: tool.server_name,
                    display_name: tool.tool_name,
                    catalog_tool,
                },
            )
        })
        .collect();
    let tools = {
        let catalog = sess
            .services
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        summarize_model_visible_tools(
            &router.model_visible_specs(),
            &catalog,
            &mcp_tools,
            &dynamic_tool_names,
            &script_tool_names,
        )
    };

    let event = Event {
        id: sub_id,
        msg: EventMsg::AllToolsResponse(AllToolsResponseEvent { tools }),
    };
    sess.send_event_raw(event).await;
}

pub async fn list_mcp_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
    let _auth = sess.services.auth_manager.auth().await;
    let config = sess.get_config().await;
    let mcp_servers = sess.services.mcp_manager.effective_servers(&config);
    let snapshot = collect_mcp_snapshot_from_registry(
        &sess.services.mcp_registry,
        compute_auth_statuses(mcp_servers.iter(), config.mcp_oauth_credentials_store_mode).await,
    )
    .await;
    let event = Event {
        id: sub_id,
        msg: EventMsg::McpListToolsResponse(snapshot),
    };
    sess.send_event_raw(event).await;
}

pub async fn list_custom_prompts(sess: &Session, sub_id: String) {
    let custom_prompts: Vec<CustomPrompt> =
        if let Some(dir) = crate::custom_prompts::default_prompts_dir() {
            crate::custom_prompts::discover_prompts_in(&dir).await
        } else {
            Vec::new()
        };

    let event = Event {
        id: sub_id,
        msg: EventMsg::ListCustomPromptsResponse(ListCustomPromptsResponseEvent { custom_prompts }),
    };
    sess.send_event_raw(event).await;
}

pub async fn dynamic_tool_response(sess: &Arc<Session>, id: String, response: DynamicToolResponse) {
    sess.notify_dynamic_tool_response(&id, response).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::groups;

    async fn receive_tool_summaries(receiver: &async_channel::Receiver<Event>) -> Vec<ToolSummary> {
        let event = receiver.recv().await.expect("all-tools response");
        let EventMsg::AllToolsResponse(response) = event.msg else {
            panic!("expected all-tools response");
        };
        response.tools
    }

    #[test]
    fn qualified_mcp_tools_keep_their_server_without_catching_mcp_helpers() {
        let catalog = Catalog::from_inventory();
        let mcp_tools = HashMap::from([(
            "mcp__example__inspect".to_string(),
            McpToolPresentation {
                server_name: "example".to_string(),
                display_name: "inspect".to_string(),
                catalog_tool: chaos_traits::catalog::CatalogTool {
                    name: "inspect".to_string(),
                    description: String::new(),
                    input_schema: serde_json::json!({}),
                    annotations: Some(serde_json::json!({
                        "readOnlyHint": true,
                        "openWorldHint": false
                    })),
                    read_only_hint: Some(true),
                    supports_parallel_tool_calls: true,
                },
            },
        )]);
        let dynamic_tool_names = HashSet::new();
        let script_tool_names = HashSet::new();

        let presentation = tool_presentation(
            "mcp__example__inspect",
            &catalog,
            &mcp_tools,
            &dynamic_tool_names,
            &script_tool_names,
        );
        assert_eq!(presentation.display_name, "inspect");
        assert_eq!(presentation.source, "mcp:example");
        assert!(
            presentation
                .annotation_labels
                .iter()
                .any(|label| label == "read-only")
        );
        assert!(presentation.annotations.is_some());

        let presentation = tool_presentation(
            "list_mcp_resources",
            &catalog,
            &mcp_tools,
            &dynamic_tool_names,
            &script_tool_names,
        );
        assert_eq!(presentation.display_name, "list_mcp_resources");
        assert_eq!(presentation.source, "builtin");
    }

    #[tokio::test]
    async fn listing_uses_model_visible_registry_for_native_and_grouped_tools() {
        let (session, turn_context, receiver) =
            crate::chaos::make_session_and_context_with_rx().await;

        list_all_tools(
            &session,
            &turn_context.config,
            "list-default-tools".to_string(),
        )
        .await;
        let tools = receive_tool_summaries(&receiver).await;
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<HashSet<_>>();

        assert!(names.contains("enable_tools"));
        assert!(names.contains("switch_mode"));
        assert!(names.contains("request_user_input"));
        assert!(!names.contains("disable_tools"));
        assert!(!names.contains("git_status"));
        assert!(!names.contains("read_file"));
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool.name == "enable_tools")
                .map(|tool| tool.source.as_str()),
            Some("builtin")
        );

        session
            .services
            .tool_group_catalog
            .set_groups_enabled(&session.services.tool_group_state, [groups::GIT], true)
            .expect("enable git tools");
        list_all_tools(
            &session,
            &turn_context.config,
            "list-enabled-tools".to_string(),
        )
        .await;
        let tools = receive_tool_summaries(&receiver).await;
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<HashSet<_>>();

        assert!(names.contains("enable_tools"));
        assert!(names.contains("disable_tools"));
        assert!(names.contains("git_status"));
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool.name == "git_status")
                .map(|tool| tool.source.as_str()),
            Some("git")
        );
    }
}
