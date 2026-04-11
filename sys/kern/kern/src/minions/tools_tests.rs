use super::*;
use crate::AuthManager;
use crate::ChaosAuth;
use crate::ProcessTable;
use crate::built_in_model_providers;
use crate::chaos::make_session_and_context;
use crate::config::DEFAULT_AGENT_MAX_DEPTH;
use crate::config::types::ShellEnvironmentPolicy;
use crate::function_tool::FunctionCallError;
use crate::protocol::ApprovalPolicy;
use crate::protocol::FileSystemSandboxPolicy;
use crate::protocol::NetworkSandboxPolicy;
use crate::protocol::Op;
use crate::protocol::SandboxPolicy;
use crate::protocol::SessionSource;
use crate::protocol::SubAgentSource;
use crate::tools::context::ToolOutput;
use crate::turn_diff_tracker::TurnDiffTracker;
use chaos_ipc::ProcessId;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::FunctionCallOutputBody;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::RolloutItem;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::timeout;

const EMPTY_MESSAGE_ERROR: &str = "Empty message can't be sent to an agent";
const MESSAGE_AND_ITEMS_CONFLICT_ERROR: &str = "Provide either message or items, but not both";
const DEPTH_LIMIT_ERROR: &str = "Agent depth limit reached. Solve the task yourself.";

struct ValidationCase {
    name: &'static str,
    tool_name: &'static str,
    args: Value,
}

fn invocation(
    session: Arc<crate::chaos::Session>,
    turn: Arc<TurnContext>,
    tool_name: &str,
    payload: ToolPayload,
) -> ToolInvocation {
    ToolInvocation {
        session,
        turn,
        tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
        call_id: "call-1".to_string(),
        tool_name: tool_name.to_string(),
        tool_namespace: None,
        payload,
    }
}

fn function_payload(args: serde_json::Value) -> ToolPayload {
    ToolPayload::Function {
        arguments: args.to_string(),
    }
}

fn process_table() -> ProcessTable {
    ProcessTable::with_models_provider_for_tests(
        ChaosAuth::from_api_key("dummy"),
        built_in_model_providers(/* openai_base_url */ None)["openai"].clone(),
    )
}

fn expect_text_output<T>(output: T) -> (String, Option<bool>)
where
    T: ToolOutput,
{
    let response = output.to_response_item(
        "call-1",
        &ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    );
    match response {
        ResponseInputItem::FunctionCallOutput { output, .. }
        | ResponseInputItem::CustomToolCallOutput { output, .. } => {
            let content = match output.body {
                FunctionCallOutputBody::Text(text) => text,
                FunctionCallOutputBody::ContentItems(items) => {
                    chaos_ipc::models::function_call_output_content_items_to_text(&items)
                        .unwrap_or_default()
                }
            };
            (content, output.success)
        }
        other => panic!("expected function output, got {other:?}"),
    }
}

fn attach_process_manager(session: &mut crate::chaos::Session) -> ProcessTable {
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    manager
}

fn set_depth_limited_sub_agent_source(session: &crate::chaos::Session, turn: &mut TurnContext) {
    let max_depth = turn.config.agent_max_depth;
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
        parent_process_id: session.conversation_id,
        depth: max_depth,
        agent_nickname: None,
        agent_role: None,
    });
}

async fn invoke_validation_handler<F, S>(
    tool_name: &'static str,
    args: Value,
    setup: F,
) -> Result<(), FunctionCallError>
where
    F: FnOnce(&mut crate::chaos::Session, &mut TurnContext) -> S,
{
    let (mut session, mut turn) = make_session_and_context().await;
    let _setup_state = setup(&mut session, &mut turn);
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        tool_name,
        function_payload(args),
    );
    match tool_name {
        "spawn_agent" => SpawnAgentHandler.handle(invocation).await.map(|_| ()),
        "send_input" => SendInputHandler.handle(invocation).await.map(|_| ()),
        "resume_agent" => ResumeAgentHandler.handle(invocation).await.map(|_| ()),
        "wait_agent" => WaitAgentHandler.handle(invocation).await.map(|_| ()),
        other => panic!("unsupported validation test tool: {other}"),
    }
}

async fn assert_respond_to_model_error<F, S>(
    case_name: &str,
    tool_name: &'static str,
    args: Value,
    expected: &str,
    setup: F,
) where
    F: FnOnce(&mut crate::chaos::Session, &mut TurnContext) -> S,
{
    let Err(err) = invoke_validation_handler(tool_name, args, setup).await else {
        panic!("{case_name} should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(expected.to_string()),
        "{case_name} should return the expected validation error",
    );
}

async fn assert_respond_to_model_error_starts_with<F, S>(
    case_name: &str,
    tool_name: &'static str,
    args: Value,
    prefix: &str,
    setup: F,
) where
    F: FnOnce(&mut crate::chaos::Session, &mut TurnContext) -> S,
{
    let Err(err) = invoke_validation_handler(tool_name, args, setup).await else {
        panic!("{case_name} should be rejected");
    };
    let FunctionCallError::RespondToModel(message) = err else {
        panic!("{case_name} should return a respond-to-model error");
    };
    assert!(
        message.starts_with(prefix),
        "{case_name} should start with {prefix:?}, got {message:?}",
    );
}

#[tokio::test]
async fn handler_rejects_non_function_payloads() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        ToolPayload::Custom {
            input: "hello".to_string(),
        },
    );
    let Err(err) = SpawnAgentHandler.handle(invocation).await else {
        panic!("payload should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "collab handler received unsupported payload".to_string()
        )
    );
}

#[tokio::test]
async fn collab_input_handlers_reject_empty_messages() {
    for case in [
        ValidationCase {
            name: "spawn_agent rejects blank message",
            tool_name: "spawn_agent",
            args: json!({"message": "   "}),
        },
        ValidationCase {
            name: "send_input rejects empty message",
            tool_name: "send_input",
            args: json!({"id": ProcessId::new().to_string(), "message": ""}),
        },
    ] {
        assert_respond_to_model_error(
            case.name,
            case.tool_name,
            case.args,
            EMPTY_MESSAGE_ERROR,
            |_, _| (),
        )
        .await;
    }
}

#[tokio::test]
async fn collab_input_handlers_reject_message_and_items_together() {
    for case in [
        ValidationCase {
            name: "spawn_agent rejects message+items",
            tool_name: "spawn_agent",
            args: json!({
                "message": "hello",
                "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
            }),
        },
        ValidationCase {
            name: "send_input rejects message+items",
            tool_name: "send_input",
            args: json!({
                "id": ProcessId::new().to_string(),
                "message": "hello",
                "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
            }),
        },
    ] {
        assert_respond_to_model_error(
            case.name,
            case.tool_name,
            case.args,
            MESSAGE_AND_ITEMS_CONFLICT_ERROR,
            |_, _| (),
        )
        .await;
    }
}

#[tokio::test]
async fn spawn_agent_uses_scout_role_and_preserves_approval_policy() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let mut config = (*turn.config).clone();
    let provider = built_in_model_providers(/* openai_base_url */ None)["openai"].clone();
    config.model_provider_id = "openai".to_string();
    config.model_provider = provider.clone();
    config
        .permissions
        .approval_policy
        .set(ApprovalPolicy::Interactive)
        .expect("approval policy should be set");
    turn.approval_policy
        .set(ApprovalPolicy::Interactive)
        .expect("approval policy should be set");
    turn.provider = provider;
    turn.config = Arc::new(config);

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "inspect this repo",
            "agent_type": "scout"
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = agent_id(&result.agent_id).expect("agent_id should be valid");
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    let snapshot = manager
        .get_process(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.approval_policy, ApprovalPolicy::Interactive);
    assert_eq!(snapshot.model_provider_id, "openai");
}

#[tokio::test]
async fn spawn_agent_errors_when_manager_dropped() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let Err(err) = SpawnAgentHandler.handle(invocation).await else {
        panic!("spawn should fail without a manager");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("collab manager unavailable".to_string())
    );
}

#[tokio::test]
async fn spawn_agent_reapplies_runtime_sandbox_after_role_config() {
    fn pick_allowed_sandbox_policy(
        constraint: &crate::config::Constrained<SandboxPolicy>,
        base: SandboxPolicy,
    ) -> SandboxPolicy {
        let candidates = [
            SandboxPolicy::RootAccess,
            SandboxPolicy::new_workspace_write_policy(),
            SandboxPolicy::new_read_only_policy(),
        ];
        candidates
            .into_iter()
            .find(|candidate| *candidate != base && constraint.can_set(candidate).is_ok())
            .unwrap_or(base)
    }

    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let expected_sandbox = pick_allowed_sandbox_policy(
        &turn.config.permissions.sandbox_policy,
        turn.config.permissions.sandbox_policy.get().clone(),
    );
    let expected_file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(&expected_sandbox, &turn.cwd);
    let expected_network_sandbox_policy = NetworkSandboxPolicy::from(&expected_sandbox);
    turn.approval_policy
        .set(ApprovalPolicy::Interactive)
        .expect("approval policy should be set");
    turn.sandbox_policy
        .set(expected_sandbox.clone())
        .expect("sandbox policy should be set");
    turn.file_system_sandbox_policy = expected_file_system_sandbox_policy.clone();
    turn.network_sandbox_policy = expected_network_sandbox_policy;
    assert_ne!(
        expected_sandbox,
        turn.config.permissions.sandbox_policy.get().clone(),
        "test requires a runtime sandbox override that differs from base config"
    );

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({
            "message": "await this command",
            "agent_type": "scout"
        })),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn_agent should succeed");
    let (content, _) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    let agent_id = agent_id(&result.agent_id).expect("agent_id should be valid");
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );

    let snapshot = manager
        .get_process(agent_id)
        .await
        .expect("spawned agent thread should exist")
        .config_snapshot()
        .await;
    assert_eq!(snapshot.sandbox_policy, expected_sandbox);
    assert_eq!(snapshot.approval_policy, ApprovalPolicy::Interactive);
    let child_thread = manager
        .get_process(agent_id)
        .await
        .expect("spawned agent thread should exist");
    let child_turn = child_thread.chaos.session.new_default_turn().await;
    assert_eq!(
        child_turn.file_system_sandbox_policy,
        expected_file_system_sandbox_policy
    );
    assert_eq!(
        child_turn.network_sandbox_policy,
        expected_network_sandbox_policy
    );
}

#[tokio::test]
async fn collab_spawn_and_resume_reject_when_depth_limit_is_exceeded() {
    for (case_name, tool_name) in [
        ("spawn_agent depth limit", "spawn_agent"),
        ("resume_agent depth limit", "resume_agent"),
    ] {
        let args = match tool_name {
            "spawn_agent" => json!({"message": "hello"}),
            "resume_agent" => json!({"id": ProcessId::new().to_string()}),
            other => panic!("unexpected tool in depth-limit test: {other}"),
        };
        assert_respond_to_model_error(
            case_name,
            tool_name,
            args,
            DEPTH_LIMIT_ERROR,
            |session, turn| {
                let manager = attach_process_manager(session);
                set_depth_limited_sub_agent_source(session, turn);
                manager
            },
        )
        .await;
    }
}

#[tokio::test]
async fn spawn_agent_allows_depth_up_to_configured_max_depth() {
    #[derive(Debug, Deserialize)]
    struct SpawnAgentResult {
        agent_id: String,
        nickname: Option<String>,
    }

    let (mut session, mut turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();

    let mut config = (*turn.config).clone();
    config.agent_max_depth = DEFAULT_AGENT_MAX_DEPTH + 1;
    turn.config = Arc::new(config);
    turn.session_source = SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
        parent_process_id: session.conversation_id,
        depth: DEFAULT_AGENT_MAX_DEPTH,
        agent_nickname: None,
        agent_role: None,
    });

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "spawn_agent",
        function_payload(json!({"message": "hello"})),
    );
    let output = SpawnAgentHandler
        .handle(invocation)
        .await
        .expect("spawn should succeed within configured depth");
    let (content, success) = expect_text_output(output);
    let result: SpawnAgentResult =
        serde_json::from_str(&content).expect("spawn_agent result should be json");
    assert!(!result.agent_id.is_empty());
    assert!(
        result
            .nickname
            .as_deref()
            .is_some_and(|nickname| !nickname.is_empty())
    );
    assert_eq!(success, Some(true));
}

#[tokio::test]
async fn collab_handlers_reject_invalid_ids() {
    for case in [
        ValidationCase {
            name: "send_input invalid id",
            tool_name: "send_input",
            args: json!({"id": "not-a-uuid", "message": "hi"}),
        },
        ValidationCase {
            name: "resume_agent invalid id",
            tool_name: "resume_agent",
            args: json!({"id": "not-a-uuid"}),
        },
        ValidationCase {
            name: "wait_agent invalid id",
            tool_name: "wait_agent",
            args: json!({"ids": ["invalid"]}),
        },
    ] {
        let invalid_id = match case.tool_name {
            "wait_agent" => "invalid",
            _ => "not-a-uuid",
        };
        assert_respond_to_model_error_starts_with(
            case.name,
            case.tool_name,
            case.args,
            &format!("invalid agent id {invalid_id}:"),
            |_, _| (),
        )
        .await;
    }
}

#[tokio::test]
async fn collab_agent_lookup_handlers_report_missing_agents() {
    for tool_name in ["send_input", "resume_agent"] {
        let agent_id = ProcessId::new();
        let args = match tool_name {
            "send_input" => json!({"id": agent_id.to_string(), "message": "hi"}),
            "resume_agent" => json!({"id": agent_id.to_string()}),
            other => panic!("unexpected tool in missing-agent test: {other}"),
        };
        assert_respond_to_model_error(
            tool_name,
            tool_name,
            args,
            &format!("agent with id {agent_id} not found"),
            |session, _| attach_process_manager(session),
        )
        .await;
    }
}

#[tokio::test]
async fn send_input_interrupts_before_prompt() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_process(config).await.expect("start thread");
    let agent_id = thread.process_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "id": agent_id.to_string(),
            "message": "hi",
            "interrupt": true
        })),
    );
    SendInputHandler
        .handle(invocation)
        .await
        .expect("send_input should succeed");

    let ops = manager.captured_ops();
    let ops_for_agent: Vec<&Op> = ops
        .iter()
        .filter_map(|(id, op)| (*id == agent_id).then_some(op))
        .collect();
    assert_eq!(ops_for_agent.len(), 2);
    assert!(matches!(ops_for_agent[0], Op::Interrupt));
    assert!(matches!(ops_for_agent[1], Op::UserInput { .. }));

    let _ = thread
        .process
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn send_input_accepts_structured_items() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_process(config).await.expect("start thread");
    let agent_id = thread.process_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "send_input",
        function_payload(json!({
            "id": agent_id.to_string(),
            "items": [
                {"type": "mention", "name": "drive", "path": "app://google_drive"},
                {"type": "text", "text": "read the folder"}
            ]
        })),
    );
    SendInputHandler
        .handle(invocation)
        .await
        .expect("send_input should succeed");

    let expected = Op::UserInput {
        items: vec![
            UserInput::Mention {
                name: "drive".to_string(),
                path: "app://google_drive".to_string(),
            },
            UserInput::Text {
                text: "read the folder".to_string(),
                text_elements: Vec::new(),
            },
        ],
        final_output_json_schema: None,
    };
    let captured = manager
        .captured_ops()
        .into_iter()
        .find(|(id, op)| *id == agent_id && *op == expected);
    assert_eq!(captured, Some((agent_id, expected)));

    let _ = thread
        .process
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn resume_agent_noops_for_active_agent() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_process(config).await.expect("start thread");
    let agent_id = thread.process_id;
    let status_before = manager.agent_control().get_status(agent_id).await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );

    let output = ResumeAgentHandler
        .handle(invocation)
        .await
        .expect("resume_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&content).expect("resume_agent result should be json");
    assert_eq!(result.status, status_before);
    assert_eq!(success, Some(true));

    let process_ids = manager.list_process_ids().await;
    assert_eq!(process_ids, vec![agent_id]);

    let _ = thread
        .process
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn resume_agent_restores_closed_agent_and_accepts_send_input() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager
        .resume_process_with_history(
            config,
            InitialHistory::Forked(vec![RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "materialized".to_string(),
                }],
                end_turn: None,
                phase: None,
            })]),
            AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("dummy")),
            false,
            None,
        )
        .await
        .expect("start thread");
    let agent_id = thread.process_id;
    let _ = manager
        .agent_control()
        .shutdown_agent(agent_id)
        .await
        .expect("shutdown agent");
    assert_eq!(
        manager.agent_control().get_status(agent_id).await,
        AgentStatus::NotFound
    );
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let resume_invocation = invocation(
        session.clone(),
        turn.clone(),
        "resume_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );
    let output = ResumeAgentHandler
        .handle(resume_invocation)
        .await
        .expect("resume_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: resume_agent::ResumeAgentResult =
        serde_json::from_str(&content).expect("resume_agent result should be json");
    assert_ne!(result.status, AgentStatus::NotFound);
    assert_eq!(success, Some(true));

    let send_invocation = invocation(
        session,
        turn,
        "send_input",
        function_payload(json!({"id": agent_id.to_string(), "message": "hello"})),
    );
    let output = SendInputHandler
        .handle(send_invocation)
        .await
        .expect("send_input should succeed after resume");
    let (content, success) = expect_text_output(output);
    let result: serde_json::Value =
        serde_json::from_str(&content).expect("send_input result should be json");
    let submission_id = result
        .get("submission_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(!submission_id.is_empty());
    assert_eq!(success, Some(true));

    let _ = manager
        .agent_control()
        .shutdown_agent(agent_id)
        .await
        .expect("shutdown resumed agent");
}

#[tokio::test]
async fn wait_agent_rejects_non_positive_timeout() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [ProcessId::new().to_string()],
            "timeout_ms": 0
        })),
    );
    let Err(err) = WaitAgentHandler.handle(invocation).await else {
        panic!("non-positive timeout should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("timeout_ms must be greater than zero".to_string())
    );
}

#[tokio::test]
async fn wait_agent_rejects_empty_ids() {
    let (session, turn) = make_session_and_context().await;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({"ids": []})),
    );
    let Err(err) = WaitAgentHandler.handle(invocation).await else {
        panic!("empty ids should be rejected");
    };
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("ids must be non-empty".to_string())
    );
}

#[tokio::test]
async fn wait_agent_returns_not_found_for_missing_agents() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let id_a = ProcessId::new();
    let id_b = ProcessId::new();
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [id_a.to_string(), id_b.to_string()],
            "timeout_ms": 1000
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        wait::WaitAgentResult {
            status: HashMap::from([(id_a, AgentStatus::NotFound), (id_b, AgentStatus::NotFound),]),
            timed_out: false
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn wait_agent_times_out_when_status_is_not_final() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_process(config).await.expect("start thread");
    let agent_id = thread.process_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [agent_id.to_string()],
            "timeout_ms": MIN_WAIT_TIMEOUT_MS
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        wait::WaitAgentResult {
            status: HashMap::new(),
            timed_out: true
        }
    );
    assert_eq!(success, None);

    let _ = thread
        .process
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_agent_clamps_short_timeouts_to_minimum() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_process(config).await.expect("start thread");
    let agent_id = thread.process_id;
    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [agent_id.to_string()],
            "timeout_ms": 10
        })),
    );

    let early = timeout(
        Duration::from_millis(50),
        WaitAgentHandler.handle(invocation),
    )
    .await;
    assert!(
        early.is_err(),
        "wait_agent should not return before the minimum timeout clamp"
    );

    let _ = thread
        .process
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
}

#[tokio::test]
async fn wait_agent_returns_final_status_without_timeout() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_process(config).await.expect("start thread");
    let agent_id = thread.process_id;
    let mut status_rx = manager
        .agent_control()
        .subscribe_status(agent_id)
        .await
        .expect("subscribe should succeed");

    let _ = thread
        .process
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");
    let _ = timeout(Duration::from_secs(1), status_rx.changed())
        .await
        .expect("shutdown status should arrive");

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "wait_agent",
        function_payload(json!({
            "ids": [agent_id.to_string()],
            "timeout_ms": 1000
        })),
    );
    let output = WaitAgentHandler
        .handle(invocation)
        .await
        .expect("wait_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: wait::WaitAgentResult =
        serde_json::from_str(&content).expect("wait_agent result should be json");
    assert_eq!(
        result,
        wait::WaitAgentResult {
            status: HashMap::from([(agent_id, AgentStatus::Shutdown)]),
            timed_out: false
        }
    );
    assert_eq!(success, None);
}

#[tokio::test]
async fn close_agent_submits_shutdown_and_returns_status() {
    let (mut session, turn) = make_session_and_context().await;
    let manager = process_table();
    session.services.agent_control = manager.agent_control();
    let config = turn.config.as_ref().clone();
    let thread = manager.start_process(config).await.expect("start thread");
    let agent_id = thread.process_id;
    let status_before = manager.agent_control().get_status(agent_id).await;

    let invocation = invocation(
        Arc::new(session),
        Arc::new(turn),
        "close_agent",
        function_payload(json!({"id": agent_id.to_string()})),
    );
    let output = CloseAgentHandler
        .handle(invocation)
        .await
        .expect("close_agent should succeed");
    let (content, success) = expect_text_output(output);
    let result: close_agent::CloseAgentResult =
        serde_json::from_str(&content).expect("close_agent result should be json");
    assert_eq!(result.status, status_before);
    assert_eq!(success, Some(true));

    let ops = manager.captured_ops();
    let submitted_shutdown = ops
        .iter()
        .any(|(id, op)| *id == agent_id && matches!(op, Op::Shutdown));
    assert_eq!(submitted_shutdown, true);

    let status_after = manager.agent_control().get_status(agent_id).await;
    assert_eq!(status_after, AgentStatus::NotFound);
}

#[tokio::test]
async fn build_agent_spawn_config_uses_turn_context_values() {
    fn pick_allowed_sandbox_policy(
        constraint: &crate::config::Constrained<SandboxPolicy>,
        base: SandboxPolicy,
    ) -> SandboxPolicy {
        let candidates = [
            SandboxPolicy::new_read_only_policy(),
            SandboxPolicy::new_workspace_write_policy(),
            SandboxPolicy::RootAccess,
        ];
        candidates
            .into_iter()
            .find(|candidate| *candidate != base && constraint.can_set(candidate).is_ok())
            .unwrap_or(base)
    }

    let (_session, mut turn) = make_session_and_context().await;
    let base_instructions = BaseInstructions {
        text: "base".to_string(),
    };
    turn.minion_instructions = Some("dev".to_string());
    turn.compact_prompt = Some("compact".to_string());
    turn.shell_environment_policy = ShellEnvironmentPolicy {
        use_profile: true,
        ..ShellEnvironmentPolicy::default()
    };
    let temp_dir = tempfile::tempdir().expect("temp dir");
    turn.cwd = temp_dir.path().to_path_buf();
    turn.alcatraz_linux_exe = Some(PathBuf::from("/bin/echo"));
    let sandbox_policy = pick_allowed_sandbox_policy(
        &turn.config.permissions.sandbox_policy,
        turn.config.permissions.sandbox_policy.get().clone(),
    );
    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy(&sandbox_policy, &turn.cwd);
    let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);
    turn.sandbox_policy
        .set(sandbox_policy)
        .expect("sandbox policy set");
    turn.file_system_sandbox_policy = file_system_sandbox_policy.clone();
    turn.network_sandbox_policy = network_sandbox_policy;
    turn.approval_policy
        .set(ApprovalPolicy::Interactive)
        .expect("approval policy set");

    let config = build_agent_spawn_config(&base_instructions, &turn).expect("spawn config");
    let mut expected = (*turn.config).clone();
    expected.base_instructions = Some(base_instructions.text);
    expected.model = Some(turn.model_info.slug.clone());
    expected.model_provider = turn.provider.clone();
    expected.model_reasoning_effort = turn.reasoning_effort;
    expected.model_reasoning_summary = Some(turn.reasoning_summary);
    expected.minion_instructions = turn.minion_instructions.clone();
    expected.compact_prompt = turn.compact_prompt.clone();
    expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
    expected.alcatraz_linux_exe = turn.alcatraz_linux_exe.clone();
    expected.cwd = turn.cwd.clone();
    expected
        .permissions
        .approval_policy
        .set(ApprovalPolicy::Interactive)
        .expect("approval policy set");
    expected
        .permissions
        .sandbox_policy
        .set(turn.sandbox_policy.get().clone())
        .expect("sandbox policy set");
    expected.permissions.file_system_sandbox_policy = file_system_sandbox_policy;
    expected.permissions.network_sandbox_policy = network_sandbox_policy;
    assert_eq!(config, expected);
}

#[tokio::test]
async fn build_agent_spawn_config_preserves_base_user_instructions() {
    let (_session, mut turn) = make_session_and_context().await;
    let mut base_config = (*turn.config).clone();
    base_config.user_instructions = Some("base-user".to_string());
    turn.user_instructions = Some("resolved-user".to_string());
    turn.config = Arc::new(base_config.clone());
    let base_instructions = BaseInstructions {
        text: "base".to_string(),
    };

    let config = build_agent_spawn_config(&base_instructions, &turn).expect("spawn config");

    assert_eq!(config.user_instructions, base_config.user_instructions);
}

#[tokio::test]
async fn build_agent_resume_config_clears_base_instructions() {
    let (_session, mut turn) = make_session_and_context().await;
    let mut base_config = (*turn.config).clone();
    base_config.base_instructions = Some("caller-base".to_string());
    turn.config = Arc::new(base_config);
    turn.approval_policy
        .set(ApprovalPolicy::Interactive)
        .expect("approval policy set");

    let config = build_agent_resume_config(&turn, 0).expect("resume config");

    let mut expected = (*turn.config).clone();
    expected.base_instructions = None;
    expected.model = Some(turn.model_info.slug.clone());
    expected.model_provider = turn.provider.clone();
    expected.model_reasoning_effort = turn.reasoning_effort;
    expected.model_reasoning_summary = Some(turn.reasoning_summary);
    expected.minion_instructions = turn.minion_instructions.clone();
    expected.compact_prompt = turn.compact_prompt.clone();
    expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
    expected.alcatraz_linux_exe = turn.alcatraz_linux_exe.clone();
    expected.cwd = turn.cwd.clone();
    expected
        .permissions
        .approval_policy
        .set(ApprovalPolicy::Interactive)
        .expect("approval policy set");
    expected
        .permissions
        .sandbox_policy
        .set(turn.sandbox_policy.get().clone())
        .expect("sandbox policy set");
    assert_eq!(config, expected);
}
