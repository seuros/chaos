use super::common::get_agent_info;
use super::common::impl_function_tool_kind;
use super::common::impl_tool_output;
use super::{
    AgentStatus, CloseAgentArgs, CollabCloseBeginEvent, CollabCloseEndEvent, FunctionCallError,
    ResponseInputItem, Serialize, ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolPayload,
    agent_id, collab_agent_error, function_arguments, parse_arguments, tool_output_json_text,
    tool_output_response_item,
};
use serde::Deserialize;

pub(crate) struct Handler;

impl ToolHandler for Handler {
    type Output = CloseAgentResult;

    impl_function_tool_kind!();

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
        let args: CloseAgentArgs = parse_arguments(&arguments)?;
        let agent_id = agent_id(&args.id)?;
        let (receiver_agent_nickname, receiver_agent_role) =
            get_agent_info(&session, agent_id).await;
        session
            .send_event(
                &turn,
                CollabCloseBeginEvent {
                    call_id: call_id.clone(),
                    sender_process_id: session.conversation_id,
                    receiver_process_id: agent_id,
                }
                .into(),
            )
            .await;
        let status = match session
            .services
            .agent_control
            .subscribe_status(agent_id)
            .await
        {
            Ok(mut status_rx) => status_rx.borrow_and_update().clone(),
            Err(err) => {
                let status = session.services.agent_control.get_status(agent_id).await;
                session
                    .send_event(
                        &turn,
                        CollabCloseEndEvent {
                            call_id: call_id.clone(),
                            sender_process_id: session.conversation_id,
                            receiver_process_id: agent_id,
                            receiver_agent_nickname: receiver_agent_nickname.clone(),
                            receiver_agent_role: receiver_agent_role.clone(),
                            status,
                        }
                        .into(),
                    )
                    .await;
                return Err(collab_agent_error(agent_id, err));
            }
        };
        let result = if !matches!(status, AgentStatus::Shutdown) {
            session
                .services
                .agent_control
                .shutdown_agent(agent_id)
                .await
                .map_err(|err| collab_agent_error(agent_id, err))
                .map(|_| ())
        } else {
            Ok(())
        };
        session
            .send_event(
                &turn,
                CollabCloseEndEvent {
                    call_id,
                    sender_process_id: session.conversation_id,
                    receiver_process_id: agent_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    status: status.clone(),
                }
                .into(),
            )
            .await;
        result?;

        Ok(CloseAgentResult { status })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct CloseAgentResult {
    pub(crate) status: AgentStatus,
}

impl_tool_output!(CloseAgentResult, "close_agent");
