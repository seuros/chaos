//! MCP tool: image — return an image from a test data URL.

use mcp_host::content::types::ImageContent;
use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::TestStdioServer;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct ImageParams {}

impl TestStdioServer {
    #[mcp_tool(name = "image", read_only = true, open_world = false)]
    async fn image(&self, _ctx: Ctx<'_>, _params: Parameters<ImageParams>) -> ToolResult {
        let data_url = std::env::var("MCP_TEST_IMAGE_DATA_URL").map_err(|err| {
            ToolError::Execution(format!("missing MCP_TEST_IMAGE_DATA_URL: {err}"))
        })?;
        let (mime_type, base64_data) = parse_data_url(&data_url).map_err(ToolError::Execution)?;
        Ok(ToolOutput::content(vec![ImageContent::new(
            base64_data.to_owned(),
            mime_type.to_owned(),
        )]))
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

#[cfg(test)]
mod tests;
