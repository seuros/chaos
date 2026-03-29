#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io;
use std::sync::Arc;

use mcp_host::content::types::ImageContent;
use mcp_host::prelude::*;
use mcp_host::registry::router::McpToolRouter;
use schemars::JsonSchema;
use serde::Deserialize;

struct TestStdioServer;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct EchoParams {
    message: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct ImageParams {}

impl TestStdioServer {
    #[mcp_tool(name = "echo")]
    async fn echo(&self, _ctx: Ctx<'_>, params: Parameters<EchoParams>) -> ToolResult {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "echo".to_string(),
            serde_json::Value::String(format!("ECHOING: {}", params.0.message)),
        );
        if let Ok(value) = std::env::var("MCP_TEST_VALUE") {
            payload.insert("env".to_string(), serde_json::Value::String(value));
        }
        Ok(ToolOutput::json(serde_json::Value::Object(payload)))
    }

    #[mcp_tool(name = "image")]
    async fn image(&self, _ctx: Ctx<'_>, _params: Parameters<ImageParams>) -> ToolResult {
        let data_url = std::env::var("MCP_TEST_IMAGE_DATA_URL").map_err(|err| {
            ToolError::Execution(format!("missing MCP_TEST_IMAGE_DATA_URL: {err}"))
        })?;
        let (mime_type, base64_data) = parse_data_url(&data_url).map_err(ToolError::Execution)?;
        Ok(ToolOutput::content(vec![Box::new(ImageContent::new(
            base64_data,
            mime_type,
        ))]))
    }
}

fn parse_data_url(data_url: &str) -> Result<(&str, &str), String> {
    let payload = data_url
        .strip_prefix("data:")
        .ok_or_else(|| "data URL must start with data:".to_string())?;
    let (metadata, base64_data) = payload
        .split_once(',')
        .ok_or_else(|| "data URL must contain a comma separator".to_string())?;
    let mime_type = metadata
        .strip_suffix(";base64")
        .ok_or_else(|| "data URL must use ;base64 encoding".to_string())?;
    if mime_type.is_empty() {
        return Err("data URL is missing a MIME type".to_string());
    }
    if base64_data.is_empty() {
        return Err("data URL is missing base64 payload".to_string());
    }
    Ok((mime_type, base64_data))
}

fn tool_router() -> McpToolRouter<TestStdioServer> {
    McpToolRouter::new()
        .with_tool(
            TestStdioServer::echo_tool_info(),
            TestStdioServer::echo_handler,
            None,
        )
        .with_tool(
            TestStdioServer::image_tool_info(),
            TestStdioServer::image_handler,
            None,
        )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let mcp_server = server("test-stdio-server", env!("CARGO_PKG_VERSION"))
        .with_tools(true)
        .build();
    let server = Arc::new(TestStdioServer);
    tool_router().register_all(mcp_server.tool_registry(), server);
    mcp_server
        .run(StdioTransport::new())
        .await
        .map_err(|err| io::Error::other(format!("mcp server error: {err}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_data_url;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_image_data_url() {
        let (mime, payload) = parse_data_url("data:image/png;base64,Zm9v").expect("parse data URL");
        assert_eq!(mime, "image/png");
        assert_eq!(payload, "Zm9v");
    }
}
