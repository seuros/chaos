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
use chaos_traits::catalog::tool_infos_to_catalog_tools_with_parallel;
use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const DOT_MCP_JSON: &str = ".mcp.json";

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DotMcpJson {
    #[serde(rename = "mcpServers")]
    mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct McpAddServerParams {
    /// Name for the MCP server entry in `.mcp.json`.
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct McpServerActionParams {
    /// Name of the MCP server entry to control.
    pub name: String,
    /// Action to apply to the named server.
    pub action: String,
}

struct McpManageServer;

impl McpManageServer {
    #[mcp_tool(
        name = "mcp_add_server",
        description = "Add an MCP server to the project `.mcp.json` and reload the active session.",
        read_only = false,
        open_world = false
    )]
    async fn mcp_add_server(
        &self,
        _ctx: Ctx<'_>,
        _params: Parameters<McpAddServerParams>,
    ) -> ToolResult {
        unreachable!("catalog driver path only");
    }

    #[mcp_tool(
        name = "mcp_server",
        description = "Enable, disable, reset, or remove an MCP server in the project `.mcp.json`, then reload the active session.",
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
                    execute_add_server(&dot_mcp_path, params)?
                }
                "mcp_server" => {
                    let params: McpServerActionParams =
                        serde_json::from_value(request.arguments)
                            .map_err(|e| format!("invalid arguments: {e}"))?;
                    execute_server_action(&dot_mcp_path, params)?
                }
                other => return Err(format!("unknown MCP management tool: {other}")),
            };

            Ok(CatalogToolResult {
                output,
                success: Some(true),
                effects: vec![CatalogToolEffect::ReloadProjectMcp],
            })
        })
    }
}

fn mcp_manage_tool_driver() -> Arc<dyn CatalogToolDriver> {
    Arc::new(McpManageToolDriver)
}

inventory::submit! {
    CatalogRegistration {
        name: "mcp",
        tools: catalog_tools,
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
        tool_driver: Some(mcp_manage_tool_driver),
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
    if doc.mcp_servers.contains_key(&params.name) {
        return Err(format!(
            "MCP server `{}` already exists in {}",
            params.name,
            path.display()
        ));
    }

    let server = build_server_config(&params)?;
    doc.mcp_servers.insert(params.name, server);
    write_dot_mcp_json(path, &doc)?;
    Ok(doc.mcp_servers)
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

fn execute_add_server(path: &Path, params: McpAddServerParams) -> Result<String, String> {
    let server_kind = match (&params.command, &params.url) {
        (Some(_), None) => "stdio",
        (None, Some(_)) => "streamable_http",
        (Some(_), Some(_)) => return Err("provide either `command` or `url`, not both".to_string()),
        (None, None) => return Err("either `command` or `url` is required".to_string()),
    };
    let server_name = params.name.clone();
    add_server_to_dot_mcp_json(path, params)?;
    Ok(format!(
        "Added MCP server `{server_name}` to {} ({server_kind}) and requested a live reload.",
        path.display(),
    ))
}

fn execute_server_action(path: &Path, params: McpServerActionParams) -> Result<String, String> {
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
            Ok(format!(
                "Marked MCP server `{}` as enabled in {} and requested a live reload.",
                params.name,
                path.display()
            ))
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
            Ok(format!(
                "Marked MCP server `{}` as disabled in {} and requested a live reload.",
                params.name,
                path.display()
            ))
        }
        "remove" => {
            doc.mcp_servers.remove(&params.name);
            write_dot_mcp_json(path, &doc)?;
            Ok(format!(
                "Removed MCP server `{}` from {} and requested a live reload.",
                params.name,
                path.display()
            ))
        }
        "reset" => Ok(format!(
            "Requested a live MCP reload for `{}` from {}.",
            params.name,
            path.display()
        )),
        other => Err(format!(
            "invalid action `{other}`; expected one of: enable, disable, reset, remove"
        )),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn add_server_creates_dot_mcp_json() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join(".mcp.json");
        let msg = execute_add_server(
            &path,
            McpAddServerParams {
                name: "docs".to_string(),
                command: Some("node".to_string()),
                args: Some(vec!["server.js".to_string()]),
                env: None,
                url: None,
                bearer_token_env_var: None,
                http_headers: None,
                enabled: Some(true),
                required: Some(false),
            },
        )
        .expect("add server");

        assert!(msg.contains("Added MCP server `docs`"));
        let doc = load_dot_mcp_json(&path).expect("reload file");
        assert!(doc.mcp_servers.contains_key("docs"));
    }

    #[test]
    fn server_action_updates_enabled_flag() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join(".mcp.json");
        execute_add_server(
            &path,
            McpAddServerParams {
                name: "docs".to_string(),
                command: Some("node".to_string()),
                args: None,
                env: None,
                url: None,
                bearer_token_env_var: None,
                http_headers: None,
                enabled: Some(true),
                required: Some(false),
            },
        )
        .expect("seed server");

        execute_server_action(
            &path,
            McpServerActionParams {
                name: "docs".to_string(),
                action: "disable".to_string(),
            },
        )
        .expect("disable");

        let doc = load_dot_mcp_json(&path).expect("reload file");
        assert!(!doc.mcp_servers["docs"].enabled);
    }

    #[test]
    fn add_server_rejects_invalid_transport_instead_of_panicking() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join(".mcp.json");
        let err = execute_add_server(
            &path,
            McpAddServerParams {
                name: "docs".to_string(),
                command: Some("node".to_string()),
                args: None,
                env: None,
                url: Some("https://example.com/mcp".to_string()),
                bearer_token_env_var: None,
                http_headers: None,
                enabled: Some(true),
                required: Some(false),
            },
        )
        .expect_err("mixed transports should fail");

        assert!(err.contains("provide either `command` or `url`, not both"));
    }

    #[test]
    fn add_http_server_persists_http_headers() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join(".mcp.json");
        execute_add_server(
            &path,
            McpAddServerParams {
                name: "docs".to_string(),
                command: None,
                args: None,
                env: None,
                url: Some("https://example.com/mcp".to_string()),
                bearer_token_env_var: None,
                http_headers: Some(BTreeMap::from([(
                    "Authorization".to_string(),
                    "Bearer token".to_string(),
                )])),
                enabled: Some(true),
                required: Some(false),
            },
        )
        .expect("add http server");

        let doc = load_dot_mcp_json(&path).expect("reload file");
        let server = &doc.mcp_servers["docs"];
        match &server.transport {
            McpServerTransportConfig::StreamableHttp { http_headers, .. } => {
                assert_eq!(
                    http_headers
                        .as_ref()
                        .and_then(|headers| headers.get("Authorization")),
                    Some(&"Bearer token".to_string())
                );
            }
            other => panic!("expected streamable http server, got {other:?}"),
        }
    }
}
