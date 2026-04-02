use crate::chaos::Session;
use crate::function_tool::FunctionCallError;
use crate::minions::exceeds_process_spawn_depth_limit;
use crate::minions::next_process_spawn_depth;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use std::sync::Arc;

/// Generates a `ToolOutput` impl for a `Serialize` result type that delegates to
/// `tool_output_json_text` and `tool_output_response_item`.
///
/// The optional third argument overrides `success_for_logging` (default `true`).
/// The optional fourth argument overrides the success value passed to
/// `tool_output_response_item` (default `Some(true)`).
///
/// ```ignore
/// impl_tool_output!(SpawnAgentResult, "spawn_agent");
/// impl_tool_output!(WaitAgentResult, "wait_agent", true, None);
/// ```
macro_rules! impl_tool_output {
    ($ty:ty, $name:expr) => {
        impl_tool_output!($ty, $name, true, Some(true));
    };
    ($ty:ty, $name:expr, $success_log:expr, $success_resp:expr) => {
        impl ToolOutput for $ty {
            fn log_preview(&self) -> String {
                tool_output_json_text(self, $name)
            }

            fn success_for_logging(&self) -> bool {
                $success_log
            }

            fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
                tool_output_response_item(call_id, payload, self, $success_resp, $name)
            }
        }
    };
}

pub(super) use impl_tool_output;

/// Generates `kind()` and `matches_kind()` for a `ToolHandler` that handles only
/// `ToolKind::Function` payloads (excluding `ToolSearch`).
///
/// Place inside the `impl ToolHandler for Handler { ... }` block:
///
/// ```ignore
/// impl ToolHandler for Handler {
///     type Output = Foo;
///     impl_function_tool_kind!();
///     async fn handle(...) { ... }
/// }
/// ```
macro_rules! impl_function_tool_kind {
    () => {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        fn matches_kind(&self, payload: &ToolPayload) -> bool {
            matches!(payload, ToolPayload::Function { .. })
        }
    };
}

pub(super) use impl_function_tool_kind;

/// Retrieves the nickname and role for an agent, returning `(None, None)` on failure.
pub(super) async fn get_agent_info(
    session: &Arc<Session>,
    process_id: ProcessId,
) -> (Option<String>, Option<String>) {
    session
        .services
        .agent_control
        .get_agent_nickname_and_role(process_id)
        .await
        .unwrap_or((None, None))
}

/// Returns an error if the child depth exceeds the configured maximum.
pub(super) fn check_depth_limit(
    session_source: &SessionSource,
    max_depth: i32,
) -> Result<i32, FunctionCallError> {
    let child_depth = next_process_spawn_depth(session_source);
    if exceeds_process_spawn_depth_limit(child_depth, max_depth) {
        return Err(FunctionCallError::RespondToModel(
            "Agent depth limit reached. Solve the task yourself.".to_string(),
        ));
    }
    Ok(child_depth)
}
