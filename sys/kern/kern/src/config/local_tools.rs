use super::McpServerConfig;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

pub(super) async fn mise_server(cwd: &Path) -> Option<McpServerConfig> {
    let mise = which::which("mise").ok()?;
    // Older mise releases don't serve MCP. Never wait on a broken shim.
    let status = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new(&mise)
            .args(["mcp", "--help"])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .ok()?
    .ok()?;
    status.success().then_some(())?;
    // Task execution and tool installation require explicit MCP configuration.
    serde_json::from_value(serde_json::json!({
        "command": mise.to_str()?,
        "args": ["mcp"],
        "cwd": cwd.to_str()?,
        "enabled_tools": ["list_commands"]
    }))
    .ok()
}
