//! MCP resource exposing cron jobs via `chaos://crons`.

use mcp_host::prelude::*;
use mcp_host::registry::router::McpResourceRouter;

use crate::chaos_tool::ChaosMcpServer;

fn crons_list_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<ResourceContent>, ResourceError>> + Send + 'a>,
> {
    let _ = ctx;
    Box::pin(async move {
        let chaos_pool = server.state_runtime.as_ref().and_then(|rt| rt.chaos_pool());

        let content = match chaos_cron::resource::list_crons(chaos_pool).await {
            Ok(json) => json,
            Err(msg) => return Err(ResourceError::Internal(msg)),
        };

        Ok(vec![text_resource_with_mime(
            "chaos://crons",
            content,
            "application/json",
        )])
    })
}

fn crons_resource_info() -> ResourceInfo {
    ResourceInfo {
        uri: "chaos://crons".to_string(),
        name: "crons".to_string(),
        description: Some("List all scheduled cron jobs".to_string()),
        mime_type: Some("application/json".to_string()),
    }
}

pub(crate) fn resource_router() -> McpResourceRouter<ChaosMcpServer> {
    McpResourceRouter::new().with_resource(crons_resource_info(), crons_list_handler, None)
}
