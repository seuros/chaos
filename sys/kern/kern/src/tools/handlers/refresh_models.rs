use serde::Deserialize;

use crate::builtin_mcp_resources::refresh_models_json;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::extract_function_arguments;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct RefreshModelsHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshModelsArgs {
    provider: String,
}

impl ToolHandler for RefreshModelsHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments = extract_function_arguments(invocation.payload, "refresh_models")?;
        let args: RefreshModelsArgs = parse_arguments(&arguments)?;
        let output = refresh_models_json(
            &invocation.session.services.models_manager,
            &invocation.turn.config.model_providers,
            &args.provider,
        )
        .await
        .map_err(FunctionCallError::RespondToModel)?;
        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}
