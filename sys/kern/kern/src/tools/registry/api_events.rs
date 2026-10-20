//! Public, bounded tool previews for native API frontends.
//!
//! Reuse the invocation event vocabulary (server=None denotes an internal
//! tool). Console/exec sessions retain their existing specialized rendering.
use std::time::Duration;

use chaos_ipc::mcp::CallToolResult;
use chaos_ipc::protocol::{
    EventMsg, McpInvocation, McpToolCallBeginEvent, McpToolCallEndEvent, SessionSource,
};

use crate::tools::context::{ToolInvocation, ToolPayload};

pub(super) struct ToolEvents(McpInvocation);

impl ToolEvents {
    pub async fn start(invocation: &ToolInvocation, public_input: &str) -> Option<Self> {
        if matches!(invocation.payload, ToolPayload::Mcp { .. })
            || !matches!(
                invocation.session.session_source().await,
                SessionSource::Api | SessionSource::Acp
            )
        {
            return None;
        }
        let tool = McpInvocation {
            server: None,
            tool: match &invocation.tool_namespace {
                Some(namespace) => format!("{namespace}.{}", invocation.tool_name),
                None => invocation.tool_name.clone(),
            },
            arguments: Some(
                serde_json::from_str(public_input)
                    .unwrap_or_else(|_| serde_json::Value::String(public_input.to_owned())),
            ),
        };
        invocation
            .session
            .send_event(
                &invocation.turn,
                EventMsg::McpToolCallBegin(McpToolCallBeginEvent {
                    call_id: invocation.call_id.clone(),
                    invocation: tool.clone(),
                }),
            )
            .await;
        Some(Self(tool))
    }

    pub async fn finish(
        self,
        invocation: &ToolInvocation,
        duration: Duration,
        success: bool,
        preview: String,
    ) {
        // This is the handler's bounded public preview, not raw provider data
        // or hidden reasoning. Full model-facing results stay kernel-owned.
        let preview = chaos_wchar::take_bytes_at_char_boundary(&preview, 16 * 1024);
        let result = CallToolResult {
            content: vec![serde_json::json!({"type":"text","text":preview})],
            structured_content: None,
            is_error: Some(!success),
            meta: None,
        };
        invocation
            .session
            .send_event(
                &invocation.turn,
                EventMsg::McpToolCallEnd(McpToolCallEndEvent {
                    call_id: invocation.call_id.clone(),
                    invocation: self.0,
                    duration,
                    result: Ok(result),
                }),
            )
            .await;
    }
}
