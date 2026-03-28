//! Arsenal driver — dispatches tool calls to `chaos_arsenal` by name.
//!
//! The kernel discovers arsenal's tools at boot via
//! `chaos_arsenal::tools::tool_infos()` and registers this single handler
//! for all of them.  The kernel never owns per-tool schemas or adapters;
//! arsenal is the sole source of truth.

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct ArsenalHandler;

impl ToolHandler for ArsenalHandler {
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

        let result = match tool_name.as_str() {
            "read_file" => chaos_arsenal::tools::read_file::execute(&args_value).await,
            "grep_files" => chaos_arsenal::tools::grep_files::execute(&args_value).await,
            "list_dir" => chaos_arsenal::tools::list_dir::execute(&args_value).await,
            other => Err(format!("unknown arsenal tool: {other}")),
        };

        match result {
            Ok(text) => Ok(FunctionToolOutput::from_text(text, Some(true))),
            Err(msg) => Err(FunctionCallError::RespondToModel(msg)),
        }
    }
}
