//! MCP handler spans use the embedding application's subscriber and exporter.
//! Never attach parameters, resource URIs, results, or error messages.

use std::future::Future;

use mcp_host::prelude::{ToolError, ToolOutput};

#[tracing::instrument(
    name = "mcp.host.tool",
    skip_all,
    fields(rpc.system = "jsonrpc", rpc.method = "tools/call", mcp.tool = name,
           otel.kind = "server", otel.status_code)
)]
pub(crate) async fn tool(
    name: &str,
    operation: impl Future<Output = Result<ToolOutput, ToolError>>,
) -> Result<ToolOutput, ToolError> {
    let result = operation.await;
    tracing::Span::current().record(
        "otel.status_code",
        if result.is_ok() { "OK" } else { "ERROR" },
    );
    result
}
