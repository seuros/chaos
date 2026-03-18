use std::path::PathBuf;
use std::sync::Arc;

use codex_core::CodexThread;
use codex_protocol::ThreadId;
use codex_protocol::parse_command::ParsedCommand;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use rmcp::model::ErrorData;
use rmcp::model::RequestId;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use tracing::error;

use crate::elicitation::ApprovalElicitationAction;
use crate::elicitation::ApprovalElicitationResponse;

/// Conforms to the MCP elicitation request params shape, so it can be used as
/// the `params` field of an `elicitation/create` request.
#[derive(Debug, Deserialize, Serialize)]
pub struct ExecApprovalElicitRequestParams {
    pub message: String,
    #[serde(rename = "requestedSchema")]
    pub requested_schema: Value,
    #[serde(rename = "_meta")]
    pub meta: ExecApprovalElicitRequestMeta,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExecApprovalElicitRequestMeta {
    #[serde(rename = "threadId")]
    pub thread_id: ThreadId,
    pub codex_elicitation: String,
    pub codex_mcp_tool_call_id: String,
    pub codex_event_id: String,
    pub codex_call_id: String,
    pub codex_command: Vec<String>,
    pub codex_cwd: PathBuf,
    pub codex_parsed_cmd: Vec<ParsedCommand>,
}

pub type ExecApprovalResponse = ApprovalElicitationResponse;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_exec_approval_request(
    command: Vec<String>,
    cwd: PathBuf,
    outgoing: Arc<crate::outgoing_message::OutgoingMessageSender>,
    codex: Arc<CodexThread>,
    request_id: RequestId,
    tool_call_id: String,
    event_id: String,
    call_id: String,
    approval_id: String,
    codex_parsed_cmd: Vec<ParsedCommand>,
    thread_id: ThreadId,
) {
    let escaped_command =
        shlex::try_join(command.iter().map(String::as_str)).unwrap_or_else(|_| command.join(" "));
    let message = format!(
        "Allow Codex to run `{escaped_command}` in `{cwd}`?",
        cwd = cwd.to_string_lossy()
    );

    let params = ExecApprovalElicitRequestParams {
        message,
        requested_schema: json!({"type":"object","properties":{}}),
        meta: ExecApprovalElicitRequestMeta {
            thread_id,
            codex_elicitation: "exec-approval".to_string(),
            codex_mcp_tool_call_id: tool_call_id.clone(),
            codex_event_id: event_id.clone(),
            codex_call_id: call_id,
            codex_command: command,
            codex_cwd: cwd,
            codex_parsed_cmd,
        },
    };
    let params_json = match serde_json::to_value(&params) {
        Ok(value) => value,
        Err(err) => {
            let message = format!("Failed to serialize ExecApprovalElicitRequestParams: {err}");
            error!("{message}");

            outgoing
                .send_error(request_id.clone(), ErrorData::invalid_params(message, None))
                .await;

            return;
        }
    };

    if !outgoing.supports_form_elicitation() {
        error!("client does not support form elicitation; denying exec approval request");
        submit_exec_approval(approval_id, event_id, ReviewDecision::Denied, codex).await;
        return;
    }

    let on_response = outgoing
        .send_request("elicitation/create", Some(params_json))
        .await;

    // Listen for the response on a separate task so we don't block the main agent loop.
    {
        let codex = codex.clone();
        let approval_id = approval_id.clone();
        let event_id = event_id.clone();
        tokio::spawn(async move {
            on_exec_approval_response(approval_id, event_id, on_response, codex).await;
        });
    }
}

async fn on_exec_approval_response(
    approval_id: String,
    event_id: String,
    receiver: tokio::sync::oneshot::Receiver<Result<serde_json::Value, ErrorData>>,
    codex: Arc<CodexThread>,
) {
    let response = receiver.await;
    let value = match response {
        Ok(Ok(value)) => value,
        Ok(Err(err)) => {
            error!("elicitation request failed: {err:?}");
            submit_exec_approval(approval_id, event_id, ReviewDecision::Denied, codex).await;
            return;
        }
        Err(err) => {
            error!("request failed: {err:?}");
            submit_exec_approval(approval_id, event_id, ReviewDecision::Denied, codex).await;
            return;
        }
    };

    // Try to deserialize `value` and then make the appropriate call to `codex`.
    let response = serde_json::from_value::<ExecApprovalResponse>(value).unwrap_or_else(|err| {
        error!("failed to deserialize ExecApprovalResponse: {err}");
        // If we cannot deserialize the response, we deny the request to be
        // conservative.
        ExecApprovalResponse {
            action: ApprovalElicitationAction::Decline,
            content: None,
            meta: None,
        }
    });

    submit_exec_approval(approval_id, event_id, response.review_decision(), codex).await;
}

async fn submit_exec_approval(
    approval_id: String,
    event_id: String,
    decision: ReviewDecision,
    codex: Arc<CodexThread>,
) {
    if let Err(err) = codex
        .submit(Op::ExecApproval {
            id: approval_id,
            turn_id: Some(event_id),
            decision,
        })
        .await
    {
        error!("failed to submit ExecApproval: {err}");
    }
}
