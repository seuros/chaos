use super::*;
use super::common::check_depth_limit;
use super::common::get_agent_info;
use super::common::impl_function_tool_kind;
use super::common::impl_tool_output;
use crate::minions::control::SpawnAgentOptions;
use crate::minions::role::DEFAULT_ROLE_NAME;
use crate::minions::role::apply_role_to_config;

pub(crate) struct Handler;

impl ToolHandler for Handler {
    type Output = SpawnAgentResult;

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
        let args: SpawnAgentArgs = parse_arguments(&arguments)?;
        let role_name = args
            .agent_type
            .as_deref()
            .map(str::trim)
            .filter(|role: &&str| !role.is_empty());
        let input_items = parse_collab_input(args.message, args.items)?;
        let prompt = input_preview(&input_items);
        let child_depth = check_depth_limit(&turn.session_source, turn.config.agent_max_depth)?;
        session
            .send_event(
                &turn,
                CollabAgentSpawnBeginEvent {
                    call_id: call_id.clone(),
                    sender_process_id: session.conversation_id,
                    prompt: prompt.clone(),
                    model: args.model.clone().unwrap_or_default(),
                    reasoning_effort: args.reasoning_effort.unwrap_or_default(),
                }
                .into(),
            )
            .await;
        let mut config =
            build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
        apply_requested_spawn_agent_model_overrides(
            &session,
            turn.as_ref(),
            &mut config,
            args.model.as_deref(),
            args.reasoning_effort,
        )
        .await?;
        apply_role_to_config(&mut config, role_name)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
        apply_spawn_agent_overrides(&mut config, child_depth);

        let result = session
            .services
            .agent_control
            .spawn_agent_with_options(
                config,
                input_items,
                Some(process_spawn_source(
                    session.conversation_id,
                    child_depth,
                    role_name,
                )),
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: args.fork_context.then(|| call_id.clone()),
                },
            )
            .await
            .map_err(collab_spawn_error);
        let (new_process_id, status) = match &result {
            Ok(process_id) => (
                Some(*process_id),
                session.services.agent_control.get_status(*process_id).await,
            ),
            Err(_) => (None, AgentStatus::NotFound),
        };
        let (new_agent_nickname, new_agent_role) = match new_process_id {
            Some(process_id) => get_agent_info(&session, process_id).await,
            None => (None, None),
        };
        let nickname = new_agent_nickname.clone();
        session
            .send_event(
                &turn,
                CollabAgentSpawnEndEvent {
                    call_id,
                    sender_process_id: session.conversation_id,
                    new_process_id,
                    new_agent_nickname,
                    new_agent_role,
                    prompt,
                    model: args.model.clone().unwrap_or_default(),
                    reasoning_effort: args.reasoning_effort.unwrap_or_default(),
                    status,
                }
                .into(),
            )
            .await;
        let new_process_id = result?;
        let role_tag = role_name.unwrap_or(DEFAULT_ROLE_NAME);
        turn.session_telemetry.counter(
            "codex.multi_agent.spawn",
            /*inc*/ 1,
            &[("role", role_tag)],
        );

        Ok(SpawnAgentResult {
            agent_id: new_process_id.to_string(),
            nickname,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SpawnAgentArgs {
    message: Option<String>,
    items: Option<Vec<UserInput>>,
    agent_type: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    fork_context: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct SpawnAgentResult {
    agent_id: String,
    nickname: Option<String>,
}

impl_tool_output!(SpawnAgentResult, "spawn_agent");
