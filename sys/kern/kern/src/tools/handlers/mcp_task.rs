use std::sync::Arc;

use chaos_mcp_runtime::McpTask as Task;
use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogTool;
use chaos_traits::catalog::ToolExposure;
use serde::Deserialize;
use serde::Serialize;

use crate::chaos::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct McpTaskHandler;

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::models::function_call_output_content_items_to_text;
    use mcp_host::protocol::types::TaskStatus;
    use serde_json::json;

    #[tokio::test]
    async fn serverless_cancel_routes_to_internal_task_store() {
        let (session, mut turn) = crate::chaos::make_session_and_context().await;
        turn.approval_policy
            .set(chaos_ipc::protocol::ApprovalPolicy::Headless)
            .unwrap();
        let session = Arc::new(session);
        let turn = Arc::new(turn);
        let task = session
            .services
            .internal_task_store
            .create_task(
                None,
                TaskStatus::Completed,
                None,
                Some(json!({"output": "done"})),
            )
            .await;
        let output = handle_cancel_task(
            session.clone(),
            turn.clone(),
            "cancel".into(),
            TaskIdArgs {
                server: None,
                task_id: task.task_id.clone(),
            },
        )
        .await
        .unwrap();
        let text = function_call_output_content_items_to_text(&output.body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["taskId"], task.task_id);
        assert!(value.get("server").is_none());
        for server in [Some("chaos_local".into()), Some("not-configured".into())] {
            assert!(
                handle_cancel_task(
                    session.clone(),
                    turn.clone(),
                    "cancel-invalid".into(),
                    TaskIdArgs {
                        server,
                        task_id: task.task_id.clone()
                    },
                )
                .await
                .is_err()
            );
        }
        assert!(serde_json::from_value::<CallToolAsyncArgs>(json!({"tool": "demo"})).is_err());
    }
}

// ── args ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CallToolAsyncArgs {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Option<serde_json::Value>,
    #[serde(default)]
    ttl: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TaskIdArgs {
    server: Option<String>,
    task_id: String,
}

// ── output shapes ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct TaskPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    server: Option<String>,
    #[serde(flatten)]
    task: Task,
}

// ── handler impl ─────────────────────────────────────────────────────────────

impl ToolHandler for McpTaskHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            tool_name,
            payload,
            ..
        } = invocation;

        let arguments_str = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "mcp_task handler received unsupported payload".to_string(),
                ));
            }
        };

        match tool_name.as_str() {
            "call_mcp_tool_async" => {
                let args: CallToolAsyncArgs = super::parse_arguments(&arguments_str)?;
                handle_call_tool_async(session, turn, call_id, args).await
            }
            "cancel_mcp_task" => {
                let args: TaskIdArgs = super::parse_arguments(&arguments_str)?;
                handle_cancel_task(session, turn, call_id, args).await
            }
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported MCP task tool: {other}"
            ))),
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn to_output<T: Serialize>(value: T) -> Result<FunctionToolOutput, FunctionCallError> {
    let text = serde_json::to_string(&value).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to serialize response: {e}"))
    })?;
    Ok(FunctionToolOutput::from_text(text, Some(true)))
}

// ── tool handlers ─────────────────────────────────────────────────────────────

/// Initiates an async MCP tool call through the full approval stack.
/// Begin/end events are emitted by `handle_mcp_tool_call_async` with the
/// actual remote tool name so the audit trail is accurate.
async fn handle_call_tool_async(
    session: Arc<crate::chaos::Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    args: CallToolAsyncArgs,
) -> Result<FunctionToolOutput, FunctionCallError> {
    super::mcp_resource::normalize_resource_server(Some(args.server.clone()))?;
    let task = crate::mcp_tool_call::handle_mcp_tool_call_async(
        session,
        &turn,
        call_id,
        args.server.clone(),
        args.tool,
        args.arguments,
        args.ttl,
    )
    .await?;

    to_output(TaskPayload {
        server: Some(args.server),
        task,
    })
}

/// Cancels a running task through the full approval stack.
/// Delegates to `handle_mcp_cancel_task` which mirrors the same policy checks
/// (approval, ARC monitor) used for async tool initiation.
async fn handle_cancel_task(
    session: Arc<crate::chaos::Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    args: TaskIdArgs,
) -> Result<FunctionToolOutput, FunctionCallError> {
    let server = super::mcp_resource::normalize_resource_server(args.server)?;
    let task = if let Some(server) = &server {
        crate::mcp_tool_call::handle_mcp_cancel_task(
            session,
            &turn,
            call_id,
            server.clone(),
            args.task_id,
        )
        .await?
    } else {
        session
            .cancel_internal_task(args.task_id.as_str())
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
    };

    to_output(TaskPayload { server, task })
}

// ── catalog registration ──────────────────────────────────────────────────────

fn mcp_task_catalog_tools() -> Vec<CatalogTool> {
    use crate::client_common::tools::ToolSpec;
    use crate::tools::spec::{create_call_mcp_tool_async_tool, create_cancel_mcp_task_tool};
    use chaos_parrot::sanitize::ResponsesApiTool;

    [
        create_call_mcp_tool_async_tool(),
        create_cancel_mcp_task_tool(),
    ]
    .into_iter()
    .filter_map(|spec| match spec {
        ToolSpec::Function(ResponsesApiTool {
            name,
            description,
            parameters,
            ..
        }) => Some(CatalogTool {
            name,
            description,
            input_schema: serde_json::to_value(parameters).unwrap_or_default(),
            annotations: None,
            read_only_hint: None,
            supports_parallel_tool_calls: false,
        }),
        _ => None,
    })
    .collect()
}

fn mcp_task_tool_exposure(_tool_name: &str) -> ToolExposure {
    ToolExposure::Always
}

inventory::submit! {
    CatalogRegistration {
        name: "mcp_task",
        tools: mcp_task_catalog_tools,
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
        tool_driver: None,
        tool_exposure: mcp_task_tool_exposure,
    }
}
