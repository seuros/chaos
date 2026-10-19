//! MCP resources exposing built-in Chaos state via shared resource definitions.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::product::OS_NAME;
use chaos_kern::builtin_mcp_resources;
use mcp_host::prelude::*;
use mcp_host::registry::router::McpResourceRouter;
use mcp_host::registry::router::McpResourceTemplateRouter;
use serde::Serialize;
use serde_json::json;

use crate::chaos_tool::ChaosMcpServer;

type ResourceReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ResourceContent>, ResourceError>> + Send + 'a>>;

fn to_json<T: Serialize>(value: &T, context: &str) -> Result<String, String> {
    serde_json::to_string(value)
        .map_err(|err| format!("failed to serialize {context} resource: {err}"))
}

struct McpHostBuiltinResourceBackend<'a> {
    server: &'a ChaosMcpServer,
    state: Arc<crate::session_state::McpSessionState>,
}

impl builtin_mcp_resources::ChaosBuiltinResourceBackend for McpHostBuiltinResourceBackend<'_> {
    async fn hooks_json(&self, id: Option<&str>) -> Result<String, String> {
        let config = &self.server.config;
        chaos_kern::hooks::resource_json(&config.chaos_home, &config.cwd, id).await
    }
    async fn machine_json(&self) -> Result<String, String> {
        let config = &self.server.config;
        builtin_mcp_resources::machine_json(config, &config.cwd).await
    }

    async fn sessions_json(&self) -> Result<String, String> {
        // The database is shared by CLI/TUI and other MCP principals. It is not
        // an ownership index and must never be used for an unfiltered listing.
        let process_ids = self.state.process_table.list_process_ids().await;
        let names = self.state.process_names.lock().await;
        let sessions = process_ids
            .iter()
            .map(|process_id| {
                json!({
                    "process_id": process_id.to_string(),
                    "title": names.get(process_id).cloned(),
                })
            })
            .collect::<Vec<_>>();
        to_json(&sessions, &format!("{OS_NAME} processes"))
    }

    async fn session_detail_json(&self, process_id: ProcessId) -> Result<String, String> {
        if !self.state.owns_process(process_id).await {
            return Err("process not owned by this MCP session".into());
        }
        if let Some(runtime_db) = self.server.runtime_db.as_ref() {
            return builtin_mcp_resources::session_detail_json_from_runtime_db(
                Some(runtime_db),
                process_id,
            )
            .await;
        }

        self.state
            .process_table
            .get_process(process_id)
            .await
            .map_err(|err| format!("process not found: {err}"))?;
        let title = self
            .state
            .process_names
            .lock()
            .await
            .get(&process_id)
            .cloned();
        to_json(
            &json!({
                "process_id": process_id.to_string(),
                "title": title,
                "status": "active",
            }),
            &format!("{OS_NAME} process"),
        )
    }

    async fn crons_json(&self) -> Result<String, String> {
        let json = chaos_cron::resource::list_crons().await?;
        let mut jobs: Vec<serde_json::Value> =
            serde_json::from_str(&json).map_err(|err| format!("invalid cron resource: {err}"))?;
        let owned = self.state.process_table.list_process_ids().await;
        jobs.retain(|job| {
            job.get("session_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| ProcessId::from_string(id).ok())
                .is_some_and(|id| owned.contains(&id))
        });
        to_json(&jobs, "owned crons")
    }

    async fn spool_json(&self) -> Result<String, String> {
        // Spool rows currently have no trustworthy session/process owner.
        // Do not expose global prompts/results until that index exists.
        Ok("[]".into())
    }

    async fn models_json(&self) -> Result<String, String> {
        let config = &self.server.config;

        let groups = self
            .state
            .process_table
            .get_models_manager()
            .list_models_by_provider(&config.model_providers, &config.model_provider_id)
            .await;
        builtin_mcp_resources::models_json_from_provider_models(&groups)
    }

    async fn modes_json(&self) -> Result<String, String> {
        let config = &self.server.config;
        builtin_mcp_resources::modes_json_from_chaos_home(&config.chaos_home).await
    }

    async fn mcp_json(&self) -> Result<String, String> {
        let config = &self.server.config;
        builtin_mcp_resources::mcp_json_from_config(None, config, None, None).await
    }
}

async fn read_builtin_resource(
    server: &ChaosMcpServer,
    session: &Session,
    uri: &str,
) -> Result<builtin_mcp_resources::ChaosBuiltinResourceContent, ResourceError> {
    let state = server
        .session_state(session)
        .map_err(|err| ResourceError::Internal(err.to_string()))?;
    let backend = McpHostBuiltinResourceBackend { server, state };
    builtin_mcp_resources::read_resource(&backend, uri)
        .await
        .map_err(ResourceError::Internal)?
        .ok_or_else(|| ResourceError::NotFound(format!("unknown {OS_NAME} resource: {uri}")))
}

fn read_static_resource_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
    uri: &'static str,
) -> ResourceReadFuture<'a> {
    Box::pin(async move {
        let content = read_builtin_resource(server, ctx.session, uri).await?;
        Ok(vec![text_resource_with_mime(
            uri,
            content.text,
            content.mime_type,
        )])
    })
}

fn sessions_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_SESSIONS_URI)
}

fn crons_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_CRONS_URI)
}

fn spool_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_SPOOL_URI)
}

fn models_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_MODELS_URI)
}

fn modes_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_MODES_URI)
}

fn mcp_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_MCP_URI)
}

fn hooks_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_HOOKS_URI)
}

fn hook_detail_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    Box::pin(async move {
        let id = ctx
            .uri_params
            .get("id")
            .ok_or_else(|| ResourceError::InvalidUri("missing hook id".into()))?;
        let uri = format!("chaos://hooks/{id}");
        let content = read_builtin_resource(server, ctx.session, &uri).await?;
        Ok(vec![text_resource_with_mime(
            uri,
            content.text,
            content.mime_type,
        )])
    })
}

fn manual_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_MANUAL_URI)
}

fn machine_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    read_static_resource_handler(server, ctx, builtin_mcp_resources::CHAOS_MACHINE_URI)
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
        let content = read_builtin_resource(server, ctx.session, &uri).await?;
        Ok(vec![text_resource_with_mime(
            uri,
            content.text,
            content.mime_type,
        )])
    })
}

fn manual_page_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    Box::pin(async move {
        let page = ctx
            .uri_params
            .get("page")
            .ok_or_else(|| ResourceError::InvalidUri("missing 'page' parameter".into()))?
            .clone();
        let uri = format!("chaos://man/{page}");
        let content = read_builtin_resource(server, ctx.session, &uri).await?;
        Ok(vec![text_resource_with_mime(
            uri,
            content.text,
            content.mime_type,
        )])
    })
}

fn resource_info(spec: &builtin_mcp_resources::ChaosBuiltinResourceSpec) -> ResourceInfo {
    ResourceInfo {
        uri: spec.uri.to_string(),
        name: spec.name.to_string(),
        title: None,
        description: Some(spec.description.to_string()),
        mime_type: Some(spec.mime_type.to_string()),
        icons: None,
        annotations: None,
        size: None,
        meta: None,
        subscribable: false,
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
        icons: None,
        annotations: None,
        meta: None,
        subscribable: false,
    }
}

pub(crate) fn resource_router() -> McpResourceRouter<ChaosMcpServer> {
    let mut router = McpResourceRouter::new();
    for spec in builtin_mcp_resources::resource_specs() {
        let handler = match spec.kind {
            builtin_mcp_resources::ChaosBuiltinResourceKind::Sessions => sessions_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Crons => crons_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Spool => spool_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Models => models_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Modes => modes_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Mcp => mcp_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Hooks => hooks_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Manual => manual_list_handler,
            builtin_mcp_resources::ChaosBuiltinResourceKind::Machine => machine_handler,
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
            builtin_mcp_resources::ChaosBuiltinResourceTemplateKind::HookDetail => {
                hook_detail_handler
            }
            builtin_mcp_resources::ChaosBuiltinResourceTemplateKind::ManualPage => {
                manual_page_handler
            }
        };
        router = router.with_template(template_info(spec), handler, None);
    }
    router
}

#[cfg(test)]
mod tests;
