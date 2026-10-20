use std::collections::HashSet;

use acp_host::protocol::{self, schema::v1::McpServer};
use chaos_kern::config::Config;
use chaos_kern::config::types::McpServerTransportConfig;

use crate::invalid;

pub(crate) fn validate_mcp(config: &Config, requested: &[McpServer]) -> protocol::Result<()> {
    let mut names = HashSet::new();
    for server in requested {
        let McpServer::Stdio(server) = server else {
            return Err(invalid(
                "only registered stdio MCP servers are accepted over ACP",
            ));
        };
        if !names.insert(&server.name) {
            return Err(invalid("duplicate ACP MCP server name"));
        }
        let configured = config
            .mcp_servers
            .get()
            .get(&server.name)
            .filter(|entry| entry.enabled)
            .ok_or_else(|| invalid("ACP MCP server is not registered and enabled in Chaos"))?;
        let McpServerTransportConfig::Stdio {
            command, args, env, ..
        } = &configured.transport
        else {
            return Err(invalid(
                "ACP MCP server does not match its registered transport",
            ));
        };
        // These declarations select existing trusted configurations. They are
        // never copied into config or passed straight to a process launcher.
        if std::path::Path::new(command) != server.command || args != &server.args {
            return Err(invalid(
                "ACP MCP command or arguments differ from registered configuration",
            ));
        }
        let mut keys = HashSet::new();
        if server.env.iter().any(|entry| {
            !keys.insert(&entry.name)
                || env.as_ref().and_then(|env| env.get(&entry.name)) != Some(&entry.value)
        }) {
            return Err(invalid(
                "ACP MCP environment differs from registered configuration",
            ));
        }
    }
    Ok(())
}
