use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::custom_prompts::CustomPrompt;
use chaos_ipc::dynamic_tools::DynamicToolResponse;
use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ListCustomPromptsResponseEvent;
use chaos_ipc::protocol::ListRemoteSkillsResponseEvent;
use chaos_ipc::protocol::ListSkillsResponseEvent;
use chaos_ipc::protocol::RemoteSkillDownloadedEvent;
use chaos_ipc::protocol::RemoteSkillHazelnutScope;
use chaos_ipc::protocol::RemoteSkillProductSurface;
use chaos_ipc::protocol::RemoteSkillSummary;
use chaos_ipc::protocol::SkillsListEntry;

use crate::chaos::Session;
use crate::config::Config;
use crate::mcp::auth::compute_auth_statuses;
use crate::mcp::collect_mcp_snapshot_from_manager;

use super::super::super::skills_info::errors_to_info;
use super::super::super::skills_info::skills_to_info;

pub async fn list_all_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
    use chaos_ipc::protocol::AllToolsResponseEvent;
    use chaos_ipc::protocol::ToolSummary;

    let mut tools: Vec<ToolSummary> = {
        let catalog = sess
            .services
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        catalog
            .tools()
            .iter()
            .map(|(source, tool)| {
                let source_str = match source {
                    crate::catalog::CatalogSource::Module(name) => name.clone(),
                    crate::catalog::CatalogSource::Mcp(name) => format!("mcp:{name}"),
                };
                let annotation_labels = tool
                    .annotations
                    .as_ref()
                    .and_then(|v| {
                        serde_json::from_value::<chaos_mcp_runtime::ToolAnnotations>(v.clone()).ok()
                    })
                    .map(|ann| {
                        let mut labels = crate::tools::spec::annotation_labels(&ann);
                        let has_read_semantics = labels
                            .iter()
                            .any(|label| label == "read-only" || label == "writes");
                        if !has_read_semantics && let Some(read_only) = tool.read_only_hint {
                            labels.push(if read_only { "read-only" } else { "writes" }.to_string());
                        }
                        labels
                    })
                    .or_else(|| {
                        tool.read_only_hint.map(|read_only| {
                            vec![if read_only { "read-only" } else { "writes" }.to_string()]
                        })
                    })
                    .unwrap_or_default();
                ToolSummary {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    annotation_labels,
                    annotations: tool.annotations.clone(),
                    source: source_str,
                }
            })
            .collect()
    };

    // Include script-defined tools from the hallucinate engine.
    if let Some(ref handle) = sess.services.hallucinate {
        for tool in handle.list_tools().await {
            tools.push(ToolSummary {
                name: tool.name,
                description: tool.description,
                annotation_labels: Vec::new(),
                annotations: None,
                source: "hallucinate".to_string(),
            });
        }
    }

    let event = Event {
        id: sub_id,
        msg: EventMsg::AllToolsResponse(AllToolsResponseEvent { tools }),
    };
    sess.send_event_raw(event).await;
}

pub async fn list_mcp_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
    let mcp_connection_manager = sess.services.mcp_connection_manager.read().await;
    let _auth = sess.services.auth_manager.auth().await;
    let config = sess.get_config().await;
    let mcp_servers = sess.services.mcp_manager.effective_servers(&config);
    let snapshot = collect_mcp_snapshot_from_manager(
        &mcp_connection_manager,
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

pub async fn list_skills(sess: &Session, sub_id: String, cwds: Vec<PathBuf>, force_reload: bool) {
    let cwds = if cwds.is_empty() {
        let state = sess.state.lock().await;
        vec![state.session_configuration.cwd.clone()]
    } else {
        cwds
    };

    let skills_manager = &sess.services.skills_manager;
    let mut skills = Vec::new();
    for cwd in cwds {
        let outcome = skills_manager.skills_for_cwd(&cwd, force_reload).await;
        let errors = errors_to_info(&outcome.errors);
        let skills_metadata = skills_to_info(&outcome.skills, &outcome.disabled_paths);
        skills.push(SkillsListEntry {
            cwd,
            skills: skills_metadata,
            errors,
        });
    }

    let event = Event {
        id: sub_id,
        msg: EventMsg::ListSkillsResponse(ListSkillsResponseEvent { skills }),
    };
    sess.send_event_raw(event).await;
}

pub async fn list_remote_skills(
    sess: &Session,
    config: &Arc<Config>,
    sub_id: String,
    hazelnut_scope: RemoteSkillHazelnutScope,
    product_surface: RemoteSkillProductSurface,
    enabled: Option<bool>,
) {
    let auth = sess.services.auth_manager.auth().await;
    let response = crate::skills::remote::list_remote_skills(
        config,
        auth.as_ref(),
        hazelnut_scope,
        product_surface,
        enabled,
    )
    .await
    .map(|skills| {
        skills
            .into_iter()
            .map(|skill| RemoteSkillSummary {
                id: skill.id,
                name: skill.name,
                description: skill.description,
            })
            .collect::<Vec<_>>()
    });

    match response {
        Ok(skills) => {
            let event = Event {
                id: sub_id,
                msg: EventMsg::ListRemoteSkillsResponse(ListRemoteSkillsResponseEvent { skills }),
            };
            sess.send_event_raw(event).await;
        }
        Err(err) => {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: format!("failed to list remote skills: {err}"),
                    chaos_error_info: Some(ChaosErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
        }
    }
}

pub async fn export_remote_skill(
    sess: &Session,
    config: &Arc<Config>,
    sub_id: String,
    hazelnut_id: String,
) {
    let auth = sess.services.auth_manager.auth().await;
    match crate::skills::remote::export_remote_skill(config, auth.as_ref(), hazelnut_id.as_str())
        .await
    {
        Ok(result) => {
            let id = result.id;
            let event = Event {
                id: sub_id,
                msg: EventMsg::RemoteSkillDownloaded(RemoteSkillDownloadedEvent {
                    id: id.clone(),
                    name: id,
                    path: result.path,
                }),
            };
            sess.send_event_raw(event).await;
        }
        Err(err) => {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: format!("failed to export remote skill {hazelnut_id}: {err}"),
                    chaos_error_info: Some(ChaosErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
        }
    }
}

pub async fn dynamic_tool_response(sess: &Arc<Session>, id: String, response: DynamicToolResponse) {
    sess.notify_dynamic_tool_response(&id, response).await;
}
