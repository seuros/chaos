use std::sync::Arc;

use chaos_traits::catalog::CatalogToolDriver;
use chaos_traits::catalog::CatalogToolRequest;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct CatalogModuleHandler {
    pub driver: Arc<dyn CatalogToolDriver>,
    pub read_only_hint: Option<bool>,
}

impl ToolHandler for CatalogModuleHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn is_mutating(&self, _invocation: &ToolInvocation) -> impl Future<Output = bool> + Send + '_ {
        async move { !matches!(self.read_only_hint, Some(true)) }
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            tool_name,
            payload,
            session,
            turn,
            ..
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

        let result = self
            .driver
            .call_tool(CatalogToolRequest {
                tool_name,
                arguments: args_value,
                cwd: turn.cwd.clone(),
                sqlite_home: turn.config.sqlite_home.clone(),
                session_id: session.conversation_id.to_string(),
            })
            .await
            .map_err(FunctionCallError::RespondToModel)?;

        Ok(FunctionToolOutput::from_text(result.output, result.success))
    }
}
