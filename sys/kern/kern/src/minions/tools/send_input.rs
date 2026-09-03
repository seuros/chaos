use super::common::get_agent_info;
use super::common::impl_function_tool_kind;
use super::common::impl_tool_output;
use super::{
    CollabAgentInteractionBeginEvent, CollabAgentInteractionEndEvent, Deserialize,
    FunctionCallError, ProcessId, ResponseInputItem, Serialize, Session, SessionSource,
    SubAgentSource, ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolPayload, TurnContext,
    UserInput, agent_id, collab_agent_error, function_arguments, input_preview, parse_arguments,
    parse_collab_input, tool_output_json_text, tool_output_response_item,
};
use std::sync::Arc;

pub(crate) struct Handler;
pub(crate) struct SupervisorHandler;

impl ToolHandler for Handler {
    type Output = SendInputResult;

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
        let args: SendInputArgs = parse_arguments(&arguments)?;
        let receiver_process_id = agent_id(&args.id)?;
        let input_items = parse_collab_input(args.message, args.items)?;
        let submission_id = send_input_to_agent(
            &session,
            &turn,
            call_id,
            receiver_process_id,
            input_items,
            args.interrupt,
        )
        .await?;

        Ok(SendInputResult { submission_id })
    }
}

impl ToolHandler for SupervisorHandler {
    type Output = SendToSupervisorResult;

    impl_function_tool_kind!();

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;
        let supervisor_process_id = match &turn.session_source {
            SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                parent_process_id, ..
            }) => *parent_process_id,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "`send_to_supervisor` is unavailable in this session.".to_string(),
                ));
            }
        };
        let arguments = function_arguments(payload)?;
        let args: SendToSupervisorArgs = parse_arguments(&arguments)?;
        let input_items = parse_collab_input(args.message, args.items)?;
        let submission_id = send_input_to_agent(
            &session,
            &turn,
            call_id,
            supervisor_process_id,
            input_items,
            false,
        )
        .await?;

        Ok(SendToSupervisorResult { submission_id })
    }
}

async fn send_input_to_agent(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    call_id: String,
    receiver_process_id: ProcessId,
    input_items: Vec<UserInput>,
    interrupt: bool,
) -> Result<String, FunctionCallError> {
    let prompt = input_preview(&input_items);
    let (receiver_agent_nickname, receiver_agent_role) =
        get_agent_info(session, receiver_process_id).await;
    if interrupt {
        session
            .services
            .agent_control
            .interrupt_agent(receiver_process_id)
            .await
            .map_err(|err| collab_agent_error(receiver_process_id, err))?;
    }
    session
        .send_event(
            turn,
            CollabAgentInteractionBeginEvent {
                call_id: call_id.clone(),
                sender_process_id: session.conversation_id,
                receiver_process_id,
                prompt: prompt.clone(),
            }
            .into(),
        )
        .await;
    let result = session
        .services
        .agent_control
        .send_input(receiver_process_id, input_items)
        .await
        .map_err(|err| collab_agent_error(receiver_process_id, err));
    let status = session
        .services
        .agent_control
        .get_status(receiver_process_id)
        .await;
    session
        .send_event(
            turn,
            CollabAgentInteractionEndEvent {
                call_id,
                sender_process_id: session.conversation_id,
                receiver_process_id,
                receiver_agent_nickname,
                receiver_agent_role,
                prompt,
                status,
            }
            .into(),
        )
        .await;
    result
}

#[derive(Debug, Deserialize)]
struct SendInputArgs {
    id: String,
    message: Option<String>,
    items: Option<Vec<UserInput>>,
    #[serde(default)]
    interrupt: bool,
}

#[derive(Debug, Deserialize)]
struct SendToSupervisorArgs {
    message: Option<String>,
    items: Option<Vec<UserInput>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SendInputResult {
    submission_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SendToSupervisorResult {
    submission_id: String,
}

impl_tool_output!(SendInputResult, "send_input");
impl_tool_output!(SendToSupervisorResult, "send_to_supervisor");
