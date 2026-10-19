#![deny(clippy::print_stdout, clippy::print_stderr)]

mod tools;

use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::sync::Arc;

use chaos_ipc::product::CHAOS_VERSION;
use mcp_host::prelude::*;

struct TestStdioServer {
    tool_registry: ToolRegistry,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    if let Some(pid_file) = std::env::var_os("MCP_TEST_PID_FILE") {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(pid_file)?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;
    }

    let mcp_server = Server::builder("test-stdio-server", CHAOS_VERSION)
        .with_tools(true)
        .build();
    let tool_registry = mcp_server.tool_registry().clone();
    let server = Arc::new(TestStdioServer {
        tool_registry: tool_registry.clone(),
    });
    for tool in tools::router().into_tools(server) {
        if tool.name() == "project_task" {
            tool_registry
                .register_boxed_configured(
                    tool,
                    ToolRegistrationPolicy::new().lifecycle(
                        ToolLifecyclePolicy::new().initial_state(ToolLifecycleState::Disabled),
                    ),
                )
                .map_err(io::Error::other)?;
        } else {
            tool_registry
                .register_boxed(tool)
                .map_err(io::Error::other)?;
        }
    }
    mcp_server
        .run(StdioTransport::new())
        .await
        .map_err(|err| io::Error::other(format!("mcp server error: {err}")))?;
    Ok(())
}
