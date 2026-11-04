mod mcp_add_server;
mod mcp_server;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::protocol::McpServerRefreshConfig;
use chaos_realpath::AbsolutePathBuf;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::McpServerTransportConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;
use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogTool;
use chaos_traits::catalog::CatalogToolDriver;
use chaos_traits::catalog::CatalogToolDriverFuture;
use chaos_traits::catalog::CatalogToolEffect;
use chaos_traits::catalog::CatalogToolRequest;
use chaos_traits::catalog::CatalogToolResult;
use chaos_traits::catalog::ToolExposure;
use chaos_traits::catalog::tool_infos_to_catalog_tools_with_parallel;
use mcp_host::prelude::{ToolInfo, ToolOutput};
use serde::Deserialize;
use serde::Serialize;

pub use mcp_add_server::McpAddServerParams;
pub use mcp_add_server::add_server_to_dot_mcp_json;
pub use mcp_server::McpServerActionParams;

pub const DOT_MCP_JSON: &str = ".mcp.json";

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DotMcpJson {
    #[serde(rename = "mcpServers")]
    mcp_servers: BTreeMap<String, McpServerConfig>,
}

struct McpManageServer;

pub fn tool_infos() -> Vec<ToolInfo> {
    vec![
        McpManageServer::mcp_add_server_tool_info(),
        McpManageServer::mcp_server_tool_info(),
    ]
}

fn mcp_server_matches_requirement(
    requirement: &crate::config_loader::McpServerRequirement,
    server: &McpServerConfig,
) -> bool {
    match &requirement.identity {
        crate::config_loader::McpServerIdentity::Command {
            command: want_command,
        } => matches!(
            &server.transport,
            McpServerTransportConfig::Stdio { command: got_command, .. }
                if got_command == want_command
        ),
        crate::config_loader::McpServerIdentity::Url { url: want_url } => matches!(
            &server.transport,
            McpServerTransportConfig::StreamableHttp { url: got_url, .. }
                if got_url == want_url
        ),
    }
}

fn filter_mcp_servers_by_requirements(
    mcp_servers: &mut HashMap<String, McpServerConfig>,
    mcp_requirements: Option<
        &crate::config_loader::Sourced<
            BTreeMap<String, crate::config_loader::McpServerRequirement>,
        >,
    >,
) {
    let Some(allowlist) = mcp_requirements else {
        return;
    };

    let source = allowlist.source.clone();
    for (name, server) in mcp_servers.iter_mut() {
        let allowed = allowlist
            .value
            .get(name)
            .is_some_and(|requirement| mcp_server_matches_requirement(requirement, server));
        if allowed {
            server.disabled_reason = None;
        } else {
            server.enabled = false;
            server.disabled_reason = Some(
                crate::config::types::McpServerDisabledReason::Requirements {
                    source: source.clone(),
                },
            );
        }
    }
}

fn catalog_tools() -> Vec<CatalogTool> {
    tool_infos_to_catalog_tools_with_parallel(tool_infos(), false)
        .into_iter()
        .map(force_write_annotations)
        .collect()
}

fn force_write_annotations(mut tool: CatalogTool) -> CatalogTool {
    tool.read_only_hint = Some(false);

    let mut annotations = tool
        .annotations
        .take()
        .unwrap_or_else(|| serde_json::json!({}));
    if let serde_json::Value::Object(map) = &mut annotations {
        map.entry("read_only_hint".to_string())
            .or_insert(serde_json::Value::Bool(false));
        map.entry("readOnlyHint".to_string())
            .or_insert(serde_json::Value::Bool(false));
    }
    tool.annotations = Some(annotations);
    tool
}

struct McpManageToolDriver;

impl CatalogToolDriver for McpManageToolDriver {
    fn call_tool(&self, request: CatalogToolRequest) -> CatalogToolDriverFuture<'_> {
        Box::pin(async move {
            let dot_mcp_path = request.project_root.join(DOT_MCP_JSON);
            let output = match request.tool_name.as_str() {
                "mcp_add_server" => {
                    let params: McpAddServerParams = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    mcp_add_server::execute_add_server(&dot_mcp_path, params)?
                }
                "mcp_server" => {
                    let params: McpServerActionParams =
                        serde_json::from_value(request.arguments)
                            .map_err(|e| format!("invalid arguments: {e}"))?;
                    mcp_server::execute_server_action(&dot_mcp_path, params)?
                }
                other => return Err(format!("unknown MCP management tool: {other}")),
            };

            Ok(CatalogToolResult {
                output: ToolOutput::structured(output).map_err(|error| {
                    format!("invalid structured MCP management output: {error}")
                })?,
                success: Some(true),
                effects: vec![CatalogToolEffect::ReloadProjectMcp],
            })
        })
    }
}

fn mcp_manage_tool_driver() -> Arc<dyn CatalogToolDriver> {
    Arc::new(McpManageToolDriver)
}

fn mcp_manage_tool_exposure(_tool_name: &str) -> ToolExposure {
    ToolExposure::groups(["mcp-management"])
}

inventory::submit! {
    CatalogRegistration {
        name: "mcp",
        tools: catalog_tools,
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
        tool_driver: Some(mcp_manage_tool_driver),
        tool_exposure: mcp_manage_tool_exposure,
    }
}

fn load_dot_mcp_json(path: &Path) -> Result<DotMcpJson, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str::<DotMcpJson>(&contents)
            .map_err(|err| format!("failed to parse {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DotMcpJson::default()),
        Err(err) => Err(format!("failed to read {}: {err}", path.display())),
    }
}

fn write_dot_mcp_json(path: &Path, doc: &DotMcpJson) -> Result<(), String> {
    let rendered = serde_json::to_string_pretty(doc)
        .map_err(|err| format!("failed to serialize {}: {err}", path.display()))?;
    std::fs::write(path, format!("{rendered}\n"))
        .map_err(|err| format!("failed to write {}: {err}", path.display()))
}

pub fn project_mcp_json_path_for_cwd(
    config_layer_stack: &crate::config_loader::ConfigLayerStack,
    cwd: &Path,
) -> std::path::PathBuf {
    crate::config_loader::project_mcp_json_path_for_stack(config_layer_stack, cwd)
}

pub fn build_project_mcp_refresh_config(
    config_layer_stack: &crate::config_loader::ConfigLayerStack,
    cwd: &Path,
    store_mode: OAuthCredentialsStoreMode,
) -> Result<McpServerRefreshConfig, String> {
    let project_mcp_json_path =
        crate::config_loader::project_mcp_json_path_for_stack(config_layer_stack, cwd);
    let layer = match std::fs::read_to_string(&project_mcp_json_path) {
        Ok(contents) => match crate::config_loader::parse_project_mcp_json(&contents) {
            Ok(config) => {
                let file =
                    AbsolutePathBuf::try_from(project_mcp_json_path.clone()).map_err(|_| {
                        format!(
                            "failed to resolve project MCP path while reloading layer: {}",
                            project_mcp_json_path.display()
                        )
                    })?;
                Some(crate::config_loader::ConfigLayerEntry::new(
                    ConfigLayerSource::ProjectMcp { file },
                    config,
                ))
            }
            Err(err) => {
                return Err(format!(
                    "failed to parse project MCP config while reloading layer: {err}"
                ));
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(format!(
                "failed to read project MCP config while reloading layer: {err}"
            ));
        }
    };

    let updated_stack = config_layer_stack.with_project_mcp_layer(layer);
    let mut mcp_servers = HashMap::new();
    for layer in updated_stack.get_layers(
        crate::config_loader::ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ false,
    ) {
        if !matches!(layer.name, ConfigLayerSource::ProjectMcp { .. }) {
            continue;
        }
        let Some(servers_value) = layer.config.get("mcp_servers") else {
            continue;
        };
        let layer_servers: HashMap<String, crate::config::types::McpServerConfig> =
            servers_value.clone().try_into().map_err(|err| {
                format!(
                    "failed to parse effective project MCP config while reloading servers: {err}"
                )
            })?;
        mcp_servers.extend(layer_servers);
    }
    filter_mcp_servers_by_requirements(
        &mut mcp_servers,
        updated_stack.requirements().mcp_servers.as_ref(),
    );

    Ok(McpServerRefreshConfig {
        mcp_servers: serde_json::to_value(mcp_servers)
            .map_err(|err| format!("failed to serialize project MCP servers: {err}"))?,
        mcp_oauth_credentials_store_mode: serde_json::to_value(store_mode)
            .map_err(|err| format!("failed to serialize MCP OAuth store mode: {err}"))?,
    })
}

#[cfg(test)]
mod tests;
