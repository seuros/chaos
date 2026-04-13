use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use async_channel::Sender;
use chaos_concierge::auth::McpAuthStatusEntry;
use chaos_ipc::protocol::Event;
use chaos_sysctl::types::McpServerTransportConfig;
use chaos_traits::McpCatalogSink;

use super::elicitation::ElicitationRequestManager;
use super::filter::StartupOutcomeError;
use super::filter::ToolFilter;

pub(super) struct MakeClientParams {
    pub(super) tool_filter: ToolFilter,
    pub(super) tx_event: Sender<Event>,
    pub(super) elicitation_requests: ElicitationRequestManager,
    pub(super) catalog: Arc<dyn McpCatalogSink>,
    pub(super) cwd: Arc<StdRwLock<PathBuf>>,
}

pub(super) fn startup_outcome_error_message(error: StartupOutcomeError) -> String {
    match error {
        StartupOutcomeError::Cancelled => "MCP startup cancelled".to_string(),
        StartupOutcomeError::Failed { error } => error,
    }
}

pub(super) fn is_mcp_client_auth_required_error(error: &StartupOutcomeError) -> bool {
    match error {
        StartupOutcomeError::Failed { error } => error.contains("Auth required"),
        _ => false,
    }
}

pub(super) fn is_mcp_client_startup_timeout_error(error: &StartupOutcomeError) -> bool {
    match error {
        StartupOutcomeError::Failed { error } => {
            error.contains("request timed out")
                || error.contains("timed out handshaking with MCP server")
        }
        _ => false,
    }
}

pub(super) fn mcp_init_error_display(
    server_name: &str,
    entry: Option<&McpAuthStatusEntry>,
    err: &StartupOutcomeError,
) -> String {
    if let Some(McpServerTransportConfig::StreamableHttp {
        url,
        bearer_token_env_var,
        http_headers,
        ..
    }) = &entry.map(|entry| &entry.config.transport)
        && url == "https://api.githubcopilot.com/mcp/"
        && bearer_token_env_var.is_none()
        && http_headers.as_ref().map(HashMap::is_empty).unwrap_or(true)
    {
        format!(
            "GitHub MCP does not support OAuth. Log in by adding a personal access token (https://github.com/settings/personal-access-tokens) to your environment and config.toml:\n[mcp_servers.{server_name}]\nbearer_token_env_var = CHAOS_GITHUB_PERSONAL_ACCESS_TOKEN"
        )
    } else if is_mcp_client_auth_required_error(err) {
        format!(
            "The {server_name} MCP server is not logged in. Run `chaos mcp login {server_name}`."
        )
    } else if is_mcp_client_startup_timeout_error(err) {
        let startup_timeout_secs = match entry {
            Some(entry) => match entry.config.startup_timeout_sec {
                Some(timeout) => timeout,
                None => super::DEFAULT_STARTUP_TIMEOUT,
            },
            None => super::DEFAULT_STARTUP_TIMEOUT,
        }
        .as_secs();
        format!(
            "MCP client for `{server_name}` timed out after {startup_timeout_secs} seconds. Add or adjust `startup_timeout_sec` in your config.toml:\n[mcp_servers.{server_name}]\nstartup_timeout_sec = XX"
        )
    } else {
        format!("MCP client for `{server_name}` failed to start: {err:#}")
    }
}

pub(super) fn transport_origin(transport: &McpServerTransportConfig) -> Option<String> {
    use url::Url;
    match transport {
        McpServerTransportConfig::StreamableHttp { url, .. } => {
            let parsed = Url::parse(url).ok()?;
            Some(parsed.origin().ascii_serialization())
        }
        McpServerTransportConfig::Stdio { .. } => Some("stdio".to_string()),
    }
}
