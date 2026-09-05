//! MCP resources exposing built-in Chaos state via shared resource definitions.

use std::future::Future;
use std::pin::Pin;

use chaos_ipc::ProcessId;
use chaos_ipc::product::OS_NAME;
use chaos_kern::builtin_mcp_resources;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigOverrides;
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

async fn load_config_for_resource(context: &str) -> Result<Config, String> {
    Config::load_with_cli_overrides_and_harness_overrides(Vec::new(), ConfigOverrides::default())
        .await
        .map_err(|err| format!("failed to load config for {context}: {err}"))
}

struct McpHostBuiltinResourceBackend<'a> {
    server: &'a ChaosMcpServer,
}

impl builtin_mcp_resources::ChaosBuiltinResourceBackend for McpHostBuiltinResourceBackend<'_> {
    async fn sessions_json(&self) -> Result<String, String> {
        if let Some(runtime_db) = self.server.runtime_db.as_ref() {
            return builtin_mcp_resources::sessions_json_from_runtime_db(Some(runtime_db)).await;
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
        to_json(&sessions, &format!("{OS_NAME} processes"))
    }

    async fn session_detail_json(&self, process_id: ProcessId) -> Result<String, String> {
        if let Some(runtime_db) = self.server.runtime_db.as_ref() {
            return builtin_mcp_resources::session_detail_json_from_runtime_db(
                Some(runtime_db),
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
        chaos_cron::resource::list_crons().await
    }

    async fn spool_json(&self) -> Result<String, String> {
        chaos_cron::resource::list_spool().await
    }

    async fn models_json(&self) -> Result<String, String> {
        // The provider map lives in config, not in the process table, so load
        // config the same way a `chaos` tool call does — minus any overrides.
        let config = load_config_for_resource("model listing").await?;

        let groups = self
            .server
            .process_table
            .get_models_manager()
            .list_models_by_provider(&config.model_providers, &config.model_provider_id)
            .await;
        builtin_mcp_resources::models_json_from_provider_models(&groups)
    }

    async fn modes_json(&self) -> Result<String, String> {
        let config = load_config_for_resource("mode listing").await?;
        builtin_mcp_resources::modes_json_from_chaos_home(&config.chaos_home)
    }

    async fn mcp_json(&self) -> Result<String, String> {
        let config = load_config_for_resource("MCP listing").await?;
        builtin_mcp_resources::mcp_json_from_config(None, &config, None, None).await
    }
}

async fn read_builtin_resource(
    server: &ChaosMcpServer,
    uri: &str,
) -> Result<builtin_mcp_resources::ChaosBuiltinResourceContent, ResourceError> {
    let backend = McpHostBuiltinResourceBackend { server };
    builtin_mcp_resources::read_resource(&backend, uri)
        .await
        .map_err(ResourceError::Internal)?
        .ok_or_else(|| ResourceError::NotFound(format!("unknown {OS_NAME} resource: {uri}")))
}

fn read_static_resource_handler<'a>(
    server: &'a ChaosMcpServer,
    uri: &'static str,
) -> ResourceReadFuture<'a> {
    Box::pin(async move {
        let content = read_builtin_resource(server, uri).await?;
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

fn spool_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    let _ = ctx;
    read_static_resource_handler(server, builtin_mcp_resources::CHAOS_SPOOL_URI)
}

fn models_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    let _ = ctx;
    read_static_resource_handler(server, builtin_mcp_resources::CHAOS_MODELS_URI)
}

fn modes_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    let _ = ctx;
    read_static_resource_handler(server, builtin_mcp_resources::CHAOS_MODES_URI)
}

fn mcp_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    let _ = ctx;
    read_static_resource_handler(server, builtin_mcp_resources::CHAOS_MCP_URI)
}

fn manual_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> ResourceReadFuture<'a> {
    let _ = ctx;
    read_static_resource_handler(server, builtin_mcp_resources::CHAOS_MANUAL_URI)
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
        let content = read_builtin_resource(server, &uri).await?;
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
        let content = read_builtin_resource(server, &uri).await?;
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
            builtin_mcp_resources::ChaosBuiltinResourceKind::Manual => manual_list_handler,
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
            builtin_mcp_resources::ChaosBuiltinResourceTemplateKind::ManualPage => {
                manual_page_handler
            }
        };
        router = router.with_template(template_info(spec), handler, None);
    }
    router
}
