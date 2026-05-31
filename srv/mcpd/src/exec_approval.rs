use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::parse_command::ParsedCommand;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ReviewDecision;
use chaos_kern::Process;
use mcp_host::protocol::types::RequestId;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use tracing::error;

use crate::elicitation::ApprovalElicitationResponse;
use crate::elicitation::create_approval_elicitation_or_deny;
use crate::elicitation::spawn_approval_response_handler;

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
    #[serde(rename = "processId")]
    pub process_id: ProcessId,
    pub codex_elicitation: String,
    pub codex_mcp_tool_call_id: String,
    pub chaos_event_id: String,
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
    process: Arc<Process>,
    request_id: RequestId,
    tool_call_id: String,
    event_id: String,
    call_id: String,
    approval_id: String,
    codex_parsed_cmd: Vec<ParsedCommand>,
    process_id: ProcessId,
) {
    let escaped_command =
        shlex::try_join(command.iter().map(String::as_str)).unwrap_or_else(|_| command.join(" "));
    let message = format!(
        "Allow Chaos to run `{escaped_command}` in `{cwd}`?",
        cwd = cwd.to_string_lossy()
    );

    let params = ExecApprovalElicitRequestParams {
        message,
        requested_schema: json!({"type":"object","properties":{}}),
        meta: ExecApprovalElicitRequestMeta {
            process_id,
            codex_elicitation: "exec-approval".to_string(),
            codex_mcp_tool_call_id: tool_call_id.clone(),
            chaos_event_id: event_id.clone(),
            codex_call_id: call_id,
            codex_command: command,
            codex_cwd: cwd,
            codex_parsed_cmd,
        },
    };
    let Some(on_response) = create_approval_elicitation_or_deny(
        outgoing.as_ref(),
        request_id.clone(),
        &params,
        "ExecApprovalElicitRequestParams",
        {
            let approval_id = approval_id.clone();
            let event_id = event_id.clone();
            let process = process.clone();
            move || async move {
                submit_exec_approval(approval_id, event_id, ReviewDecision::Denied, process).await;
            }
        },
    )
    .await
    else {
        return;
    };

    // Listen for the response on a separate task so we don't block the main agent loop.
    spawn_approval_response_handler(on_response, "ExecApprovalResponse", {
        let process = process.clone();
        let approval_id = approval_id.clone();
        let event_id = event_id.clone();
        move |decision| async move {
            submit_exec_approval(approval_id, event_id, decision, process).await;
        }
    });
}

async fn submit_exec_approval(
    approval_id: String,
    event_id: String,
    decision: ReviewDecision,
    process: Arc<Process>,
) {
    if let Err(err) = process
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
