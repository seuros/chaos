//! MCP tool: mcp_add_server — add a project MCP server.

use std::collections::BTreeMap;
use std::path::Path;

use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::McpServerTransportConfig;
use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use super::McpManageServer;
use super::load_dot_mcp_json;
use super::write_dot_mcp_json;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct McpAddServerParams {
    /// Name for the MCP server.
    pub name: String,
    /// Command to launch a stdio MCP server.
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments for a stdio MCP server.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Environment variables for a stdio MCP server.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    /// URL for a streamable HTTP MCP server.
    #[serde(default)]
    pub url: Option<String>,
    /// Optional environment variable containing a bearer token for a streamable HTTP server.
    #[serde(default)]
    pub bearer_token_env_var: Option<String>,
    /// Optional static HTTP headers for a streamable HTTP server.
    #[serde(default)]
    pub http_headers: Option<BTreeMap<String, String>>,
    /// Whether the server should start enabled. Defaults to true.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Whether failure to start this server should be treated as fatal. Defaults to false.
    #[serde(default)]
    pub required: Option<bool>,
}

impl McpManageServer {
    #[mcp_tool(
        name = "mcp_add_server",
        description = "Add an MCP server to the project's .mcp.json.",
        read_only = false,
        destructive = false,
        open_world = false
    )]
    async fn mcp_add_server(
        &self,
        _ctx: Ctx<'_>,
        _params: Parameters<McpAddServerParams>,
    ) -> ToolResult {
        unreachable!("catalog driver path only");
    }
}

fn build_server_config(params: &McpAddServerParams) -> Result<McpServerConfig, String> {
    let transport = match (&params.command, &params.url) {
        (Some(command), None) => {
            if params.http_headers.is_some() {
                return Err("`http_headers` is only supported with `url`".to_string());
            }

            McpServerTransportConfig::Stdio {
                command: command.clone(),
                args: params.args.clone().unwrap_or_default(),
                env: params.env.clone().map(|vars| {
                    vars.into_iter()
                        .collect::<std::collections::HashMap<_, _>>()
                }),
                env_vars: Vec::new(),
                cwd: None,
            }
        }
        (None, Some(url)) => McpServerTransportConfig::StreamableHttp {
            url: url.clone(),
            bearer_token: None,
            bearer_token_env_var: params.bearer_token_env_var.clone(),
            http_headers: params.http_headers.clone().map(|headers| {
                headers
                    .into_iter()
                    .collect::<std::collections::HashMap<_, _>>()
            }),
            env_http_headers: None,
        },
        (Some(_), Some(_)) => {
            return Err("provide either `command` or `url`, not both".to_string());
        }
        (None, None) => {
            return Err("either `command` or `url` is required".to_string());
        }
    };
    let transport_type = match &transport {
        McpServerTransportConfig::Stdio { .. } => "stdio",
        McpServerTransportConfig::StreamableHttp { .. } => "streamable_http",
    };

    Ok(McpServerConfig {
        transport,
        enabled: params.enabled.unwrap_or(true),
        required: params.required.unwrap_or(false),
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth_resource: None,
        r#type: Some(transport_type.to_string()),
        oauth: None,
    })
}

pub fn add_server_to_dot_mcp_json(
    path: &Path,
    params: McpAddServerParams,
) -> Result<BTreeMap<String, McpServerConfig>, String> {
    let mut doc = load_dot_mcp_json(path)?;
    if let Some(existing) = doc.mcp_servers.get(&params.name) {
        let state = if existing.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let name = &params.name;
        return Err(format!(
            "MCP server `{name}` already exists in {} (project entry, {state}). \
             In a trusted project, project entries take precedence over same-named global \
             servers even when disabled. \
             To use a same-named global server, remove the project entry with \
             `mcp_server(name: {name:?}, action: \"remove\")`.",
            path.display(),
        ));
    }

    let server = build_server_config(&params)?;
    doc.mcp_servers.insert(params.name, server);
    write_dot_mcp_json(path, &doc)?;
    Ok(doc.mcp_servers)
}

pub(super) fn execute_add_server(
    path: &Path,
    params: McpAddServerParams,
) -> Result<serde_json::Value, String> {
    let server_kind = match (&params.command, &params.url) {
        (Some(_), None) => "stdio",
        (None, Some(_)) => "streamable_http",
        (Some(_), Some(_)) => return Err("provide either `command` or `url`, not both".to_string()),
        (None, None) => return Err("either `command` or `url` is required".to_string()),
    };
    let server_name = params.name.clone();
    add_server_to_dot_mcp_json(path, params)?;
    Ok(serde_json::json!({
        "status": "added",
        "server": server_name,
        "path": path.display().to_string(),
        "transport": server_kind,
        "reload_requested": true,
    }))
}
