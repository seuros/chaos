//! Halluacinate driver — dispatches tool calls to user-defined Lua/WASM scripts.
//!
//! Script tools are discovered at session startup from `~/.config/chaos/scripts/`
//! and `.chaos/scripts/`. Each script can register tools via `chaos.tool()`.
//! This handler routes the model's tool calls to the halluacinate engine thread.

use chaos_halluacinate::HalluacinateHandle;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::extract_function_arguments;
use crate::tools::handlers::parse_json_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct HalluacinateHandler {
    pub handle: HalluacinateHandle,
}

impl ToolHandler for HalluacinateHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            tool_name, payload, ..
        } = invocation;

        let arguments = extract_function_arguments(payload, &tool_name)?;
        let args_value = parse_json_arguments(&arguments)?;

        let result = self.handle.call_tool(&tool_name, args_value).await;

        if result.success {
            Ok(FunctionToolOutput::from_text(result.output, Some(true)))
        } else {
            Err(FunctionCallError::RespondToModel(result.output))
        }
    }
}
