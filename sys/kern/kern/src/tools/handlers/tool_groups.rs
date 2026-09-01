use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::registry::{ToolHandler, ToolKind};

#[derive(Debug, Deserialize)]
struct ToolGroupsArgs {
    groups: Vec<String>,
}

pub struct ToolGroupsHandler;

impl ToolHandler for ToolGroupsHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let args: ToolGroupsArgs = match invocation.payload {
            crate::tools::context::ToolPayload::Function { arguments } => {
                super::parse_arguments(&arguments)?
            }
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "tool group handler received unsupported payload".to_string(),
                ));
            }
        };
        if args.groups.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "`groups` must contain at least one group".to_string(),
            ));
        }

        let enabled = match invocation.tool_name.as_str() {
            "enable_tools" => true,
            "disable_tools" => false,
            other => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unsupported tool group control: {other}"
                )));
            }
        };
        let change = invocation
            .session
            .services
            .tool_group_catalog
            .set_groups_enabled(
                &invocation.session.services.tool_group_state,
                args.groups,
                enabled,
            )
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
        let output = serde_json::to_string(&change).map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "failed to serialize tool group change: {error}"
            ))
        })?;
        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}
