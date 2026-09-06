//! Explicit model discovery; resource reads remain cache-only.

use chaos_kern::builtin_mcp_resources::refresh_models_json;
use chaos_kern::config::Config;
use mcp_host::prelude::*;
use mcp_host::registry::router::McpToolRouter;
use mcp_host::registry::router::tool_info_with_output;
use serde::Deserialize;
use serde_json::json;

use crate::chaos_tool::ChaosMcpServer;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshModelsParams {
    provider: String,
}

fn tool_info() -> ToolInfo {
    tool_info_with_output(
        "refresh_models",
        None,
        Some("Force a fresh model catalog fetch for a configured provider, updating its cache. Equivalent to chaos --provider <provider> models --refresh. Does not start or switch a session. Read chaos://models for cached catalogs.".to_string()),
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Configured provider ID."
                }
            },
            "required": ["provider"],
            "additionalProperties": false
        }),
        json!({
            "type": "object",
            "properties": {
                "provider": {"type": "string"},
                "models": {"type": "array", "items": {"type": "object"}}
            },
            "required": ["provider", "models"]
        }),
    )
}

fn handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>
{
    Box::pin(async move {
        let params: RefreshModelsParams = serde_json::from_value(ctx.params.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let config = Config::load_with_cli_overrides(Vec::new())
            .await
            .map_err(|err| ToolError::Execution(format!("failed to load config: {err}")))?;
        let output = refresh_models_json(
            &server.process_table.get_models_manager(),
            &config.model_providers,
            &params.provider,
        )
        .await
        .map_err(ToolError::Execution)?;
        let value: serde_json::Value = serde_json::from_str(&output)
            .map_err(|err| ToolError::Execution(format!("invalid model catalog JSON: {err}")))?;
        ToolOutput::structured(value).map_err(|err| ToolError::Execution(err.to_string()))
    })
}

pub(crate) fn tool_router() -> McpToolRouter<ChaosMcpServer> {
    McpToolRouter::new().with_tool(tool_info(), handler, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_models_requires_an_explicit_provider() {
        assert!(serde_json::from_value::<RefreshModelsParams>(json!({})).is_err());
        assert!(
            serde_json::from_value::<RefreshModelsParams>(
                json!({"provider": "charm", "command": "ignored"})
            )
            .is_err()
        );
        let params: RefreshModelsParams =
            serde_json::from_value(json!({"provider": "charm"})).unwrap();
        assert_eq!(params.provider, "charm");
        let info = serde_json::to_value(tool_info()).unwrap();
        assert_eq!(info["name"], "refresh_models");
        assert_eq!(info["inputSchema"]["required"], json!(["provider"]));
        assert_eq!(
            info["outputSchema"]["required"],
            json!(["provider", "models"])
        );
    }
}
