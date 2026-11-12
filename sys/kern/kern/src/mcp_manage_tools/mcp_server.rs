//! MCP tool: mcp_server — reconnect a session server or edit a project server.

use std::path::Path;

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use super::McpManageServer;
use super::load_dot_mcp_json;
use super::write_dot_mcp_json;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct McpServerActionParams {
    /// Name of the MCP server entry to control.
    pub name: String,
    /// Action: enable, disable, reset, or remove. Reset reconnects the named
    /// effective server (global or project) in this session only, without changing
    /// configuration or credentials. Other actions edit the project's .mcp.json.
    pub action: String,
}

impl McpManageServer {
    #[mcp_tool(
        name = "mcp_server",
        description = "Reset a global or project MCP server in this session only. \
            Enable/disable/remove edit project config. Reset reconnects only the named \
            effective server, preserves identity and saved credentials, and never enables \
            disabled servers.",
        read_only = false,
        open_world = false
    )]
    async fn mcp_server(
        &self,
        _ctx: Ctx<'_>,
        _params: Parameters<McpServerActionParams>,
    ) -> ToolResult {
        unreachable!("catalog driver path only");
    }
}

pub(super) fn execute_server_action(
    path: &Path,
    params: McpServerActionParams,
) -> Result<serde_json::Value, String> {
    let mut doc = load_dot_mcp_json(path)?;
    if !doc.mcp_servers.contains_key(&params.name) {
        return Err(format!(
            "No MCP server named `{}` found in {}",
            params.name,
            path.display()
        ));
    }

    match params.action.as_str() {
        "enable" => {
            let Some(server) = doc.mcp_servers.get_mut(&params.name) else {
                return Err(format!(
                    "No MCP server named `{}` found in {}",
                    params.name,
                    path.display()
                ));
            };
            server.enabled = true;
            write_dot_mcp_json(path, &doc)?;
            Ok(serde_json::json!({
                "status": "enabled",
                "action": "enable",
                "server": params.name,
                "path": path.display().to_string(),
                "reload_requested": true,
            }))
        }
        "disable" => {
            let Some(server) = doc.mcp_servers.get_mut(&params.name) else {
                return Err(format!(
                    "No MCP server named `{}` found in {}",
                    params.name,
                    path.display()
                ));
            };
            server.enabled = false;
            write_dot_mcp_json(path, &doc)?;
            Ok(serde_json::json!({
                "status": "disabled",
                "action": "disable",
                "server": params.name,
                "path": path.display().to_string(),
                "reload_requested": true,
            }))
        }
        "remove" => {
            doc.mcp_servers.remove(&params.name);
            write_dot_mcp_json(path, &doc)?;
            Ok(serde_json::json!({
                "status": "removed",
                "action": "remove",
                "server": params.name,
                "path": path.display().to_string(),
                "reload_requested": true,
            }))
        }
        other => Err(format!(
            "invalid project action `{other}`; expected one of: enable, disable, remove"
        )),
    }
}
