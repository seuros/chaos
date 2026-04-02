use super::common::check_depth_limit;
use super::common::get_agent_info;
use super::common::impl_function_tool_kind;
use super::common::impl_tool_output;
use super::*;

pub(crate) struct Handler;

impl ToolHandler for Handler {
    type Output = ResumeAgentResult;

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
        let args: ResumeAgentArgs = parse_arguments(&arguments)?;
        let receiver_process_id = agent_id(&args.id)?;
        let (receiver_agent_nickname, receiver_agent_role) =
            get_agent_info(&session, receiver_process_id).await;
        let child_depth = check_depth_limit(&turn.session_source, turn.config.agent_max_depth)?;

        session
            .send_event(
                &turn,
                CollabResumeBeginEvent {
                    call_id: call_id.clone(),
                    sender_process_id: session.conversation_id,
                    receiver_process_id,
                    receiver_agent_nickname: receiver_agent_nickname.clone(),
                    receiver_agent_role: receiver_agent_role.clone(),
                }
                .into(),
            )
            .await;

        let mut status = session
            .services
            .agent_control
            .get_status(receiver_process_id)
            .await;
        let error = if matches!(status, AgentStatus::NotFound) {
            match try_resume_closed_agent(&session, &turn, receiver_process_id, child_depth).await {
                Ok(resumed_status) => {
                    status = resumed_status;
                    None
                }
                Err(err) => {
                    status = session
                        .services
                        .agent_control
                        .get_status(receiver_process_id)
                        .await;
                    Some(err)
                }
            }
        } else {
            None
        };

        let (receiver_agent_nickname, receiver_agent_role) = session
            .services
            .agent_control
            .get_agent_nickname_and_role(receiver_process_id)
            .await
            .unwrap_or((receiver_agent_nickname, receiver_agent_role));
        session
            .send_event(
                &turn,
                CollabResumeEndEvent {
                    call_id,
                    sender_process_id: session.conversation_id,
                    receiver_process_id,
                    receiver_agent_nickname,
                    receiver_agent_role,
                    status: status.clone(),
                }
                .into(),
            )
            .await;

        if let Some(err) = error {
            return Err(err);
        }
        turn.session_telemetry
            .counter("codex.multi_agent.resume", /*inc*/ 1, &[]);

        Ok(ResumeAgentResult { status })
    }
}

#[derive(Debug, Deserialize)]
struct ResumeAgentArgs {
    id: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ResumeAgentResult {
    pub(crate) status: AgentStatus,
}

impl_tool_output!(ResumeAgentResult, "resume_agent");

async fn try_resume_closed_agent(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    receiver_process_id: ProcessId,
    child_depth: i32,
) -> Result<AgentStatus, FunctionCallError> {
    let config = build_agent_resume_config(turn.as_ref(), child_depth)?;
    let resumed_process_id = session
        .services
        .agent_control
        .resume_agent_from_rollout(
            config,
            receiver_process_id,
            process_spawn_source(
                session.conversation_id,
                child_depth,
                /*agent_role*/ None,
            ),
        )
        .await
        .map_err(|err| collab_agent_error(receiver_process_id, err))?;

    Ok(session
        .services
        .agent_control
        .get_status(resumed_process_id)
        .await)
}
