//! MCP resources exposing Chaos sessions via `chaos://sessions`.
//!
//! Uses the shared StateRuntime singleton for thread metadata queries.

use mcp_host::prelude::*;
use mcp_host::registry::router::{McpResourceRouter, McpResourceTemplateRouter};
use serde_json::json;

use crate::chaos_tool::ChaosMcpServer;

// ---------------------------------------------------------------------------
// Resource: chaos://sessions
// ---------------------------------------------------------------------------

fn sessions_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<ResourceContent>, ResourceError>> + Send + 'a>,
> {
    let _ = ctx;
    Box::pin(async move {
        let sessions: Vec<serde_json::Value> = match &server.state_runtime {
            Some(rt) => {
                let page = rt
                    .list_threads(
                        50,
                        None,
                        codex_state::SortKey::UpdatedAt,
                        &[],
                        None,
                        false,
                        None,
                    )
                    .await
                    .map(|p| p.items)
                    .unwrap_or_default();
                page.iter()
                    .map(|t| {
                        json!({
                            "thread_id": t.id.to_string(),
                            "title": t.title,
                            "source": t.source,
                            "cwd": t.cwd,
                            "updated_at": t.updated_at.to_rfc3339(),
                            "tokens_used": t.tokens_used,
                        })
                    })
                    .collect()
            }
            None => {
                let thread_ids = server.thread_manager.list_thread_ids().await;
                let names = server.thread_names.lock().await;
                thread_ids
                    .iter()
                    .map(|id| {
                        let name = names.get(id).cloned();
                        json!({ "thread_id": id.to_string(), "title": name })
                    })
                    .collect()
            }
        };

        let content = serde_json::to_string_pretty(&sessions).unwrap_or_default();
        Ok(vec![text_resource_with_mime(
            "chaos://sessions",
            content,
            "application/json",
        )])
    })
}

fn sessions_resource_info() -> ResourceInfo {
    ResourceInfo {
        uri: "chaos://sessions".to_string(),
        name: "sessions".to_string(),
        description: Some("List all Chaos threads".to_string()),
        mime_type: Some("application/json".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Resource template: chaos://sessions/{id}
// ---------------------------------------------------------------------------

fn session_detail_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<ResourceContent>, ResourceError>> + Send + 'a>,
> {
    Box::pin(async move {
        let id = ctx
            .uri_params
            .get("id")
            .ok_or_else(|| ResourceError::InvalidUri("missing 'id' parameter".into()))?
            .clone();

        let thread_id = codex_protocol::ThreadId::from_string(&id)
            .map_err(|e| ResourceError::NotFound(format!("invalid thread_id: {e}")))?;

        let info = match &server.state_runtime {
            Some(rt) => {
                let t = rt
                    .get_thread(thread_id)
                    .await
                    .ok()
                    .flatten()
                    .ok_or_else(|| ResourceError::NotFound(format!("thread not found: {id}")))?;
                json!({
                    "thread_id": t.id.to_string(),
                    "title": t.title,
                    "source": t.source,
                    "cwd": t.cwd,
                    "created_at": t.created_at.to_rfc3339(),
                    "updated_at": t.updated_at.to_rfc3339(),
                    "model_provider": t.model_provider,
                    "sandbox_policy": t.sandbox_policy,
                    "approval_mode": t.approval_mode,
                    "tokens_used": t.tokens_used,
                    "first_user_message": t.first_user_message,
                    "git_branch": t.git_branch,
                })
            }
            None => {
                server
                    .thread_manager
                    .get_thread(thread_id)
                    .await
                    .map_err(|e| ResourceError::NotFound(format!("thread not found: {e}")))?;
                let title = server.thread_names.lock().await.get(&thread_id).cloned();
                json!({
                    "thread_id": thread_id.to_string(),
                    "title": title,
                    "status": "active",
                })
            }
        };

        Ok(vec![text_resource_with_mime(
            format!("chaos://sessions/{id}"),
            serde_json::to_string_pretty(&info).unwrap_or_default(),
            "application/json",
        )])
    })
}

fn session_template_info() -> ResourceTemplateInfo {
    ResourceTemplateInfo {
        uri_template: "chaos://sessions/{id}".to_string(),
        name: "session_detail".to_string(),
        title: None,
        description: Some("Details for a specific Chaos thread".to_string()),
        mime_type: Some("application/json".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

pub(crate) fn resource_router() -> McpResourceRouter<ChaosMcpServer> {
    McpResourceRouter::new().with_resource(sessions_resource_info(), sessions_list_handler, None)
}

pub(crate) fn resource_template_router() -> McpResourceTemplateRouter<ChaosMcpServer> {
    McpResourceTemplateRouter::new().with_template(
        session_template_info(),
        session_detail_handler,
        None,
    )
}
