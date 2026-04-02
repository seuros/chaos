use super::*;
use super::common::get_agent_info;
use super::common::impl_function_tool_kind;
use super::common::impl_tool_output;

pub(crate) struct Handler;

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
        let prompt = input_preview(&input_items);
        let (receiver_agent_nickname, receiver_agent_role) =
            get_agent_info(&session, receiver_process_id).await;
        if args.interrupt {
            session
                .services
                .agent_control
                .interrupt_agent(receiver_process_id)
                .await
                .map_err(|err| collab_agent_error(receiver_process_id, err))?;
        }
        session
            .send_event(
                &turn,
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
                &turn,
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
        let submission_id = result?;

        Ok(SendInputResult { submission_id })
    }
}

#[derive(Debug, Deserialize)]
struct SendInputArgs {
    id: String,
    message: Option<String>,
    items: Option<Vec<UserInput>>,
    #[serde(default)]
    interrupt: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct SendInputResult {
    submission_id: String,
}

impl_tool_output!(SendInputResult, "send_input");
