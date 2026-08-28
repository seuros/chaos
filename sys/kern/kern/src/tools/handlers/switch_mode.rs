use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::extract_function_arguments;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct SwitchModeHandler;

#[derive(Debug, Deserialize)]
struct SwitchModeArgs {
    mode_id: String,
}

impl ToolHandler for SwitchModeHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let arguments = extract_function_arguments(payload, "switch_mode")?;
        let args: SwitchModeArgs = parse_arguments(&arguments)?;
        let result = session
            .switch_mode(&args.mode_id, turn.as_ref())
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        let output = serde_json::to_string(&result).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize switch_mode result: {err}"))
        })?;
        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}
