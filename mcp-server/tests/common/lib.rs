mod mcp_process;
mod mock_model_server;
mod responses;

pub use core_test_support::format_with_current_shell;
pub use core_test_support::format_with_current_shell_display_non_login;
pub use core_test_support::format_with_current_shell_non_login;
use mcp_host::protocol::types::JsonRpcResponse;
pub use mcp_process::McpProcess;
pub use mock_model_server::create_mock_responses_server;
pub use responses::create_apply_patch_sse_response;
pub use responses::create_final_assistant_message_sse_response;
pub use responses::create_shell_command_sse_response;
use serde::de::DeserializeOwned;

pub fn to_response<T: DeserializeOwned>(response: JsonRpcResponse) -> anyhow::Result<T> {
    let result = response
        .result
        .ok_or_else(|| anyhow::anyhow!("response has no result"))?;
    let value = serde_json::to_value(result)?;
    let codex_response = serde_json::from_value(value)?;
    Ok(codex_response)
}
