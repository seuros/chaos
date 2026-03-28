//! MCP resources exposing built-in Chaos state via shared resource definitions.

use std::future::Future;
use std::pin::Pin;

use chaos_ipc::ProcessId;
use chaos_kern::builtin_mcp_resources;
use mcp_host::prelude::*;
use mcp_host::registry::router::{McpResourceRouter, McpResourceTemplateRouter};
use serde::Serialize;
use serde_json::json;

use crate::chaos_tool::ChaosMcpServer;

type ResourceReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ResourceContent>, ResourceError>> + Send + 'a>>;

fn to_pretty_json<T: Serialize>(value: &T, context: &str) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map_err(|err| format!("failed to serialize {context} resource: {err}"))
}

struct McpHostBuiltinResourceBackend<'a> {
    server: &'a ChaosMcpServer,
}

impl builtin_mcp_resources::ChaosBuiltinResourceBackend for McpHostBuiltinResourceBackend<'_> {
    async fn sessions_json(&self) -> Result<String, String> {
        if let Some(state_runtime) = self.server.state_runtime.as_ref() {
            return builtin_mcp_resources::sessions_json_from_state_db(Some(state_runtime)).await;
        }

        let process_ids = self.server.process_table.list_process_ids().await;
        let names = self.server.process_names.lock().await;
        let sessions = process_ids
            .iter()
            .map(|process_id| {
                json!({
                    "process_id": process_id.to_string(),
                    "title": names.get(process_id).cloned(),
                })
            })
            .collect::<Vec<_>>();
        to_pretty_json(&sessions, "Chaos processes")
    }

    async fn session_detail_json(&self, process_id: ProcessId) -> Result<String, String> {
        if let Some(state_runtime) = self.server.state_runtime.as_ref() {
            return builtin_mcp_resources::session_detail_json_from_state_db(
                Some(state_runtime),
                process_id,
            )
            .await;
        }

        self.server
            .process_table
            .get_process(process_id)
            .await
            .map_err(|err| format!("process not found: {err}"))?;
        let title = self
            .server
            .process_names
            .lock()
            .await
            .get(&process_id)
            .cloned();
        to_pretty_json(
            &json!({
                "process_id": process_id.to_string(),
                "title": title,
                "status": "active",
            }),
            "Chaos process",
        )
    }

    async fn crons_json(&self) -> Result<String, String> {
        let chaos_pool = self
            .server
            .state_runtime
            .as_ref()
            .and_then(|rt| rt.chaos_pool());
        builtin_mcp_resources::crons_json_from_pool(chaos_pool).await
    }
}

async fn read_builtin_resource_json(
    server: &ChaosMcpServer,
    uri: &str,
) -> Result<String, ResourceError> {
    let backend = McpHostBuiltinResourceBackend { server };
    builtin_mcp_resources::read_resource_json(&backend, uri)
        .await
        .map_err(ResourceError::Internal)?
        .ok_or_else(|| ResourceError::NotFound(format!("unknown Chaos resource: {uri}")))
}

fn read_static_resource_handler<'a>(
    server: &'a ChaosMcpServer,
    uri: &'static str,
) -> ResourceReadFuture<'a> {
    Box::pin(async move {
        let content = read_builtin_resource_json(server, uri).await?;
        Ok(vec![text_resource_with_mime(
            uri,
            content,
            builtin_mcp_resources::JSON_MIME_TYPE,
        )])
    })
}

fn sessions_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    let _ = ctx;
    read_static_resource_handler(server, builtin_mcp_resources::CHAOS_SESSIONS_URI)
}

fn crons_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    let _ = ctx;
    read_static_resource_handler(server, builtin_mcp_resources::CHAOS_CRONS_URI)
}

fn session_detail_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    Box::pin(async move {
        let id = ctx
            .uri_params
            .get("id")
            .ok_or_else(|| ResourceError::InvalidUri("missing 'id' parameter".into()))?
            .clone();
        ProcessId::from_string(&id)
            .map_err(|err| ResourceError::NotFound(format!("invalid process_id: {err}")))?;
        let uri = format!("chaos://sessions/{id}");
        let content = read_builtin_resource_json(server, &uri).await?;
        Ok(vec![text_resource_with_mime(
            uri,
            content,
            builtin_mcp_resources::JSON_MIME_TYPE,
        )])
    })
}

fn resource_info(spec: &builtin_mcp_resources::ChaosBuiltinResourceSpec) -> ResourceInfo {
    ResourceInfo {
        uri: spec.uri.to_string(),
        name: spec.name.to_string(),
        description: Some(spec.description.to_string()),
        mime_type: Some(spec.mime_type.to_string()),
    }
}

fn template_info(
    spec: &builtin_mcp_resources::ChaosBuiltinResourceTemplateSpec,
) -> ResourceTemplateInfo {
    ResourceTemplateInfo {
        uri_template: spec.uri_template.to_string(),
        name: spec.name.to_string(),
        title: None,
        description: Some(spec.description.to_string()),
        mime_type: Some(spec.mime_type.to_string()),
    }
}

pub(crate) fn resource_router() -> McpResourceRouter<ChaosMcpServer> {
    let mut router = McpResourceRouter::new();
    for spec in builtin_mcp_resources::resource_specs() {
        let handler = match spec.kind {
            builtin_mcp_resources::ChaosBuiltinResourceKind::Sessions => sessions_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Crons => crons_list_handler,
        };
        router = router.with_resource(resource_info(spec), handler, None);
    }
    router
}

pub(crate) fn resource_template_router() -> McpResourceTemplateRouter<ChaosMcpServer> {
    let mut router = McpResourceTemplateRouter::new();
    for spec in builtin_mcp_resources::resource_template_specs() {
        let handler = match spec.kind {
            builtin_mcp_resources::ChaosBuiltinResourceTemplateKind::SessionDetail => {
                session_detail_handler
            }
        };
        router = router.with_template(template_info(spec), handler, None);
    }
    router
}
