use std::sync::Arc;

use chaos_traits::catalog::CatalogToolDriver;
use chaos_traits::catalog::CatalogToolEffect;
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

    #[allow(clippy::manual_async_fn)]
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

        let config = session.get_config().await;
        let project_root = crate::config_loader::project_mcp_json_path_for_stack(
            &config.config_layer_stack,
            &turn.cwd,
        )
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| turn.cwd.clone());

        let result = self
            .driver
            .call_tool(CatalogToolRequest {
                tool_name,
                arguments: args_value,
                cwd: turn.cwd.clone(),
                project_root,
                sqlite_home: turn.config.sqlite_home.clone(),
                session_id: session.conversation_id.to_string(),
            })
            .await
            .map_err(FunctionCallError::RespondToModel)?;

        for effect in &result.effects {
            match effect {
                CatalogToolEffect::ReloadProjectMcp => {
                    session
                        .reload_project_mcp_layer_and_refresh(turn.as_ref())
                        .await;
                }
            }
        }

        Ok(FunctionToolOutput::from_text(result.output, result.success))
    }
}
