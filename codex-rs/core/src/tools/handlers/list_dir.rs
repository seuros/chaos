//! Thin adapter: delegates to `chaos_arsenal::tools::list_dir::execute()`.
//!
//! The full implementation lives in the `chaos-arsenal` crate.
//! This module preserves the `ToolHandler` dispatch interface for core.

use async_trait::async_trait;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct ListDirHandler;

#[async_trait]
impl ToolHandler for ListDirHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "list_dir handler received unsupported payload".to_string(),
                ));
            }
        };

        let args_value: serde_json::Value = serde_json::from_str(&arguments)
            .map_err(|e| FunctionCallError::RespondToModel(format!("invalid JSON arguments: {e}")))?;

        match chaos_arsenal::tools::list_dir::execute(&args_value).await {
            Ok(text) => Ok(FunctionToolOutput::from_text(text, Some(true))),
            Err(msg) => Err(FunctionCallError::RespondToModel(msg)),
        }
    }
}

#[cfg(test)]
#[path = "list_dir_tests.rs"]
mod tests;
