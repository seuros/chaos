//! Cron driver — dispatches tool calls to `chaos_cron` by name.
//!
//! Mirrors the arsenal pattern: the kernel discovers cron's tools at boot via
//! `chaos_cron::tool_infos()` and registers this handler for all of them.
//! The handler pulls the chaos DB pool from the session's StateRuntime or opens
//! the shared chaos DB on demand.

use async_trait::async_trait;

use crate::function_tool::FunctionCallError;
use crate::state_db::resolve_chaos_pool;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct CronHandler;

#[async_trait]
impl ToolHandler for CronHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
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

        // Get the chaos DB pool from the session's StateRuntime.
        let existing_chaos_pool = session
            .state_db()
            .and_then(|db| db.chaos_pool().map(std::borrow::ToOwned::to_owned));
        let chaos_pool =
            resolve_chaos_pool(existing_chaos_pool, turn.config.sqlite_home.as_path()).await;

        // Build owner context from the current session/turn for scope isolation.
        let owner = chaos_cron::OwnerContext {
            project_path: Some(turn.cwd.to_string_lossy().to_string()),
            session_id: Some(session.conversation_id.to_string()),
        };

        let result = match tool_name.as_str() {
            "cron_create" => {
                let params: chaos_cron::tools::create::CronCreateParams =
                    serde_json::from_value(args_value)
                        .map_err(|e| format!("invalid arguments: {e}"))
                        .map_err(FunctionCallError::RespondToModel)?;
                chaos_cron::tools::create::execute(&params, chaos_pool.as_ref(), &owner).await
            }
            "cron_toggle" => {
                let params: chaos_cron::tools::toggle::CronToggleParams =
                    serde_json::from_value(args_value)
                        .map_err(|e| format!("invalid arguments: {e}"))
                        .map_err(FunctionCallError::RespondToModel)?;
                chaos_cron::tools::toggle::execute(&params, chaos_pool.as_ref()).await
            }
            other => Err(format!("unknown cron tool: {other}")),
        };

        match result {
            Ok(text) => Ok(FunctionToolOutput::from_text(text, Some(false))),
            Err(msg) => Err(FunctionCallError::RespondToModel(msg)),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::state_db::resolve_chaos_pool;

    #[tokio::test]
    async fn resolves_chaos_pool_on_demand_for_persistent_sessions() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        let pool = resolve_chaos_pool(None, temp_dir.path()).await;

        assert!(
            pool.is_some(),
            "persistent sessions should open chaos db lazily"
        );
        assert!(
            tokio::fs::try_exists(&chaos_proc::chaos_db_path(temp_dir.path()))
                .await
                .expect("stat chaos db"),
            "on-demand recovery should create the chaos db file"
        );
    }

    #[tokio::test]
    async fn resolves_chaos_pool_on_demand_for_ephemeral_sessions_too() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        let pool = resolve_chaos_pool(None, temp_dir.path()).await;

        assert!(pool.is_some(), "cron should always use the shared chaos db");
        assert!(
            tokio::fs::try_exists(&chaos_proc::chaos_db_path(temp_dir.path()))
                .await
                .expect("stat chaos db"),
            "on-demand recovery should create the chaos db file"
        );
    }
}
