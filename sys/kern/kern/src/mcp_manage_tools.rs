mod mcp_add_server;
mod mcp_server;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chaos_sysctl::types::McpServerConfig;
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
            let (output, effect) = match request.tool_name.as_str() {
                "mcp_add_server" => {
                    let params: McpAddServerParams = serde_json::from_value(request.arguments)
                        .map_err(|e| format!("invalid arguments: {e}"))?;
                    (
                        mcp_add_server::execute_add_server(&dot_mcp_path, params)?,
                        CatalogToolEffect::ReloadProjectMcp,
                    )
                }
                "mcp_server" => {
                    let params: McpServerActionParams =
                        serde_json::from_value(request.arguments)
                            .map_err(|e| format!("invalid arguments: {e}"))?;
                    if params.action == "reset" {
                        (
                            serde_json::json!({
                                "status": "reset",
                                "action": "reset",
                                "server": params.name,
                                "scope": "session",
                            }),
                            CatalogToolEffect::ResetMcpServer { name: params.name },
                        )
                    } else {
                        (
                            mcp_server::execute_server_action(&dot_mcp_path, params)?,
                            CatalogToolEffect::ReloadProjectMcp,
                        )
                    }
                }
                other => return Err(format!("unknown MCP management tool: {other}")),
            };

            Ok(CatalogToolResult {
                output: ToolOutput::structured(output).map_err(|error| {
                    format!("invalid structured MCP management output: {error}")
                })?,
                success: Some(true),
                effects: vec![effect],
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

#[cfg(test)]
mod tests;
