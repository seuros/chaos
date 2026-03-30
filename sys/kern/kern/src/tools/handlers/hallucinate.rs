//! Hallucinate driver — dispatches tool calls to user-defined Lua/WASM scripts.
//!
//! Script tools are discovered at session startup from `~/.config/chaos/scripts/`
//! and `.chaos/scripts/`. Each script can register tools via `chaos.tool()`.
//! This handler routes the model's tool calls to the hallucinate engine thread.

use chaos_hallucinate::HallucinateHandle;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct HallucinateHandler {
    pub handle: HallucinateHandle,
}

impl ToolHandler for HallucinateHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            tool_name, payload, ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "{tool_name} handler received unsupported payload"
                )));
            }
        };

        let args_value: serde_json::Value = serde_json::from_str(&arguments).map_err(|e| {
            FunctionCallError::RespondToModel(format!("invalid JSON arguments: {e}"))
        })?;

        let result = self.handle.call_tool(&tool_name, args_value).await;

        if result.success {
            Ok(FunctionToolOutput::from_text(result.output, Some(true)))
        } else {
            Err(FunctionCallError::RespondToModel(result.output))
        }
    }
}
