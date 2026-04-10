use super::*;
use crate::ChaosAuth;
use crate::config::ConfigBuilder;
use crate::config::test_config;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::ConfigLayerStackOrdering;
use crate::config_loader::NetworkConstraints;
use crate::config_loader::RequirementSource;
use crate::config_loader::Sourced;
use crate::exec::ExecToolCallOutput;
use crate::function_tool::FunctionCallError;
use crate::models_manager::model_info;
use crate::shell::default_user_shell;
use crate::tools::format_exec_output_str;

use chaos_ipc::ProcessId;
use chaos_ipc::models::FunctionCallOutputBody;
use chaos_ipc::models::FunctionCallOutputPayload;
use chaos_ipc::permissions::FileSystemAccessMode;
use chaos_ipc::permissions::FileSystemPath;
use chaos_ipc::permissions::FileSystemSandboxEntry;
use chaos_ipc::permissions::FileSystemSandboxPolicy;
use chaos_ipc::permissions::FileSystemSpecialPath;
use chaos_ipc::protocol::ReadOnlyAccess;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::request_permissions::PermissionGrantScope;
use chaos_ipc::request_permissions::RequestPermissionProfile;
use chaos_mcp_runtime::McpConnectionManager;
use tracing::Span;

use crate::protocol::CompactedItem;
use crate::protocol::CreditsSnapshot;
use crate::protocol::InitialHistory;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::RateLimitWindow;
use crate::protocol::ResumedHistory;
use crate::protocol::TokenCountEvent;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use crate::protocol::TurnCompleteEvent;
use crate::protocol::UserMessageEvent;
use crate::rollout::policy::EventPersistenceMode;
use crate::rollout::recorder::RolloutRecorder;
use crate::rollout::recorder::RolloutRecorderParams;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use crate::tools::ToolRouter;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::ShellHandler;
use crate::tools::handlers::UnifiedExecHandler;
use crate::tools::registry::ToolHandler;
use crate::tools::router::ToolCallSource;
use crate::turn_diff_tracker::TurnDiffTracker;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::Submission;
use chaos_ipc::protocol::W3cTraceContext;
use chaos_pf::NetworkProxyConfig;
use chaos_selinux::Decision;
use chaos_selinux::NetworkRuleProtocol;
use chaos_selinux::Policy;
use chaos_syslog::TelemetryAuthMode;
use rama::telemetry::opentelemetry::sdk::trace::SdkTracerProvider;
use rama::telemetry::opentelemetry::trace::TraceContextExt;
use rama::telemetry::opentelemetry::trace::TraceId;
use rama::telemetry::opentelemetry::trace::TracerProvider as _;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;
use tokio::time::sleep;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::*;

use chaos_ipc::mcp::CallToolResult as McpCallToolResult;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration as StdDuration;

use chaos_ipc::models::function_call_output_content_items_to_text;

fn expect_text_tool_output(output: &FunctionToolOutput) -> String {
    function_call_output_content_items_to_text(&output.body).unwrap_or_default()
}

struct InstructionsTestCase {
    slug: &'static str,
    expects_apply_patch_instructions: bool,
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

#[expect(dead_code)]
fn skill_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn developer_input_texts(items: &[ResponseItem]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "developer" => {
                Some(content.as_slice())
            }
            _ => None,
        })
        .flat_map(|content| content.iter())
        .filter_map(|item| match item {
            ContentItem::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn initial_replay_event_msgs_converts_response_items_for_resume() {
    let process_id = ProcessId::new();
    let initial_history = InitialHistory::Resumed(ResumedHistory {
        conversation_id: process_id,
        history: vec![
            RolloutItem::ResponseItem(user_message("hello from user")),
            RolloutItem::ResponseItem(assistant_message("assistant reply")),
        ],
    });

    let replay = super::initial_replay_event_msgs(&initial_history, process_id)
        .expect("expected replay events");

    assert_eq!(replay.len(), 2);
    assert!(matches!(
        &replay[0],
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::UserMessage(_),
            ..
        })
    ));
    assert!(matches!(
        &replay[1],
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::AgentMessage(_),
            ..
        })
    ));
}

#[test]
fn initial_replay_event_msgs_preserves_non_message_response_items() {
    let process_id = ProcessId::new();
    let initial_history = InitialHistory::Resumed(ResumedHistory {
        conversation_id: process_id,
        history: vec![
            RolloutItem::ResponseItem(ResponseItem::Reasoning {
                id: "reasoning_1".to_string(),
                summary: vec![
                    chaos_ipc::models::ReasoningItemReasoningSummary::SummaryText {
                        text: "think".to_string(),
                    },
                ],
                content: Some(vec![
                    chaos_ipc::models::ReasoningItemContent::ReasoningText {
                        text: "details".to_string(),
                    },
                ]),
                encrypted_content: None,
            }),
            RolloutItem::ResponseItem(ResponseItem::WebSearchCall {
                id: Some("ws_1".to_string()),
                status: Some("completed".to_string()),
                action: Some(chaos_ipc::models::WebSearchAction::Search {
                    query: Some("weather".to_string()),
                    queries: None,
                }),
            }),
            RolloutItem::ResponseItem(ResponseItem::ImageGenerationCall {
                id: "img_1".to_string(),
                status: "completed".to_string(),
                revised_prompt: Some("cat".to_string()),
                result: "image-bytes".to_string(),
            }),
        ],
    });

    let replay = super::initial_replay_event_msgs(&initial_history, process_id)
        .expect("expected replay events");

    assert_eq!(replay.len(), 3);
    assert!(matches!(
        &replay[0],
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::Reasoning(_),
            ..
        })
    ));
    assert!(matches!(
        &replay[1],
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::WebSearch(_),
            ..
        })
    ));
    assert!(matches!(
        &replay[2],
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::ImageGeneration(_),
            ..
        })
    ));
}

fn test_tool_runtime(session: Arc<Session>, turn_context: Arc<TurnContext>) -> ToolCallRuntime {
    let router = Arc::new(ToolRouter::from_config(
        &turn_context.tools_config,
        crate::tools::router::ToolRouterParams {
            mcp_tools: None,
            app_tools: None,
            dynamic_tools: turn_context.dynamic_tools.as_slice(),
            catalog_tools: vec![],
            hallucinate: None,
            plan_mode: false,
        },
    ));
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    ToolCallRuntime::new(router, session, turn_context, tracker)
}

async fn wait_for_process_rolled_back(
    rx: &async_channel::Receiver<Event>,
) -> crate::protocol::ProcessRolledBackEvent {
    let deadline = StdDuration::from_secs(2);
    let start = std::time::Instant::now();
    let mut last_event = None;
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for event; last_event={last_event:?}"))
            .expect("event");
        if let EventMsg::Error(payload) = &evt.msg
            && payload.chaos_error_info == Some(ChaosErrorInfo::ProcessRollbackFailed)
        {
            panic!("rollback emitted error instead of success: {payload:?}");
        }
        last_event = Some(evt.msg.clone());
        match evt.msg {
            EventMsg::ProcessRolledBack(payload) => return payload,
            _ => continue,
        }
    }
}

async fn wait_for_process_rollback_failed(rx: &async_channel::Receiver<Event>) -> ErrorEvent {
    let deadline = StdDuration::from_secs(2);
    let start = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        match evt.msg {
            EventMsg::Error(payload)
                if payload.chaos_error_info == Some(ChaosErrorInfo::ProcessRollbackFailed) =>
            {
                return payload;
            }
            _ => continue,
        }
    }
}

async fn attach_rollout_recorder(session: &Arc<Session>) -> ProcessId {
    let config = session.get_config().await;
    let process_id = ProcessId::default();
    let recorder = RolloutRecorder::new(
        config.as_ref(),
        RolloutRecorderParams::new(
            process_id,
            None,
            SessionSource::Exec,
            BaseInstructions::default(),
            Vec::new(),
            EventPersistenceMode::Limited,
        ),
        None,
        None,
    )
    .await
    .expect("create rollout recorder");
    {
        let mut rollout = session.services.rollout.lock().await;
        *rollout = Some(recorder);
    }
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await;
    process_id
}

fn text_block(s: &str) -> serde_json::Value {
    json!({
        "type": "text",
        "text": s,
    })
}

fn init_test_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("chaos-tests");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        tracing::subscriber::set_global_default(subscriber)
            .expect("global tracing subscriber should only be installed once");
    });
}

async fn build_test_config(chaos_home: &Path) -> Config {
    ConfigBuilder::default()
        .chaos_home(chaos_home.to_path_buf())
        .build()
        .await
        .expect("load default test config")
}

fn session_telemetry(
    conversation_id: ProcessId,
    config: &Config,
    model_info: &ModelInfo,
    session_source: SessionSource,
) -> SessionTelemetry {
    SessionTelemetry::new(
        conversation_id,
        ModelsManager::get_model_offline_for_tests(config.model.as_deref()).as_str(),
        model_info.slug.as_str(),
        None,
        Some("test@test.com".to_string()),
        Some(TelemetryAuthMode::Chatgpt),
        "test_originator".to_string(),
        false,
        "test".to_string(),
        session_source,
    )
}

pub(crate) async fn make_session_configuration_for_tests() -> SessionConfiguration {
    let chaos_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(chaos_home.path()).await;
    let config = Arc::new(config);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            minion_instructions: None,
        },
    };

    SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        minion_instructions: config.minion_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,

        cwd: config.cwd.clone(),
        chaos_home: config.chaos_home.clone(),
        process_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
    }
}

pub(crate) async fn make_session_and_context() -> (Session, TurnContext) {
    let (tx_event, _rx_event) = async_channel::unbounded();
    let chaos_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(chaos_home.path()).await;
    let config = Arc::new(config);
    let conversation_id = ProcessId::default();
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let models_manager = Arc::new(ModelsManager::new(
        config.chaos_home.clone(),
        auth_manager.clone(),
        None,
        CollaborationModesConfig::default(),
    ));
    let agent_control = AgentControl::default();
    let exec_policy = ExecPolicyManager::default();
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            minion_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        minion_instructions: config.minion_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,

        cwd: config.cwd.clone(),
        chaos_home: config.chaos_home.clone(),
        process_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
    };
    let per_turn_config = Session::build_per_turn_config(&session_configuration);
    let model_info = ModelsManager::construct_model_info_offline_for_tests(
        session_configuration.collaboration_mode.model(),
        &per_turn_config,
    );
    let session_telemetry = session_telemetry(
        conversation_id,
        config.as_ref(),
        &model_info,
        session_configuration.session_source.clone(),
    );

    let state = SessionState::new(session_configuration.clone());
    let mcp_manager = Arc::new(McpManager::new());
    let skills_manager = Arc::new(SkillsManager::new(config.chaos_home.clone(), true));
    let network_approval = Arc::new(NetworkApprovalService::default());

    let file_watcher = Arc::new(FileWatcher::noop());
    let services = SessionServices {
        catalog: Arc::new(crate::catalog::CatalogSink::new(
            crate::catalog::Catalog::from_inventory(),
        )),
        mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::new_uninitialized(
            &config.permissions.approval_policy,
        ))),
        mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
        unified_exec_manager: UnifiedExecProcessManager::new(
            config.background_terminal_max_timeout,
        ),
        hooks: Hooks::new(HooksConfig {
            legacy_notify_argv: config.notify.clone(),
            ..HooksConfig::default()
        }),
        rollout: Mutex::new(None),
        user_shell: Arc::new(default_user_shell()),
        shell_snapshot_tx: watch::channel(None).0,

        exec_policy,
        auth_manager: auth_manager.clone(),
        session_telemetry: session_telemetry.clone(),
        models_manager: Arc::clone(&models_manager),
        tool_approvals: Mutex::new(ApprovalStore::default()),
        skills_manager,
        mcp_manager,
        file_watcher,
        agent_control,
        network_proxy: None,
        network_approval: Arc::clone(&network_approval),
        runtime_db: None,
        model_client: ModelClient::new(
            Some(auth_manager.clone()),
            conversation_id,
            session_configuration.provider.clone(),
            session_configuration.session_source.clone(),
            session_configuration.approval_policy.value(),
            config.model_verbosity,
            true, // request compression always enabled
            Session::build_model_client_beta_features_header(config.as_ref()),
        ),
        hallucinate: None,
    };

    let skills_outcome = Arc::new(services.skills_manager.skills_for_config(&per_turn_config));
    let turn_context = Session::make_turn_context(
        Some(Arc::clone(&auth_manager)),
        &session_telemetry,
        session_configuration.provider.clone(),
        &session_configuration,
        per_turn_config,
        model_info,
        &models_manager,
        None,
        "turn_id".to_string(),
        skills_outcome,
    );

    let session = Session {
        conversation_id,
        tx_event,
        agent_status: agent_status_tx,
        out_of_band_elicitation_paused: watch::channel(false).0,
        state: Mutex::new(state),
        features: config.features.clone(),
        pending_mcp_server_refresh_config: Mutex::new(None),

        active_turn: Mutex::new(None),

        services,
        next_internal_sub_id: AtomicU64::new(0),
    };

    (session, turn_context)
}

pub(crate) async fn make_session_and_context_with_dynamic_tools_and_rx(
    dynamic_tools: Vec<DynamicToolSpec>,
) -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
) {
    let (tx_event, rx_event) = async_channel::unbounded();
    let chaos_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(chaos_home.path()).await;
    let config = Arc::new(config);
    let conversation_id = ProcessId::default();
    let auth_manager = AuthManager::from_auth_for_testing(ChaosAuth::from_api_key("Test API Key"));
    let models_manager = Arc::new(ModelsManager::new(
        config.chaos_home.clone(),
        auth_manager.clone(),
        None,
        CollaborationModesConfig::default(),
    ));
    let agent_control = AgentControl::default();
    let exec_policy = ExecPolicyManager::default();
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let model = ModelsManager::get_model_offline_for_tests(config.model.as_deref());
    let model_info = ModelsManager::construct_model_info_offline_for_tests(model.as_str(), &config);
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            minion_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        minion_instructions: config.minion_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        sandbox_policy: config.permissions.sandbox_policy.clone(),
        file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
        network_sandbox_policy: config.permissions.network_sandbox_policy,

        cwd: config.cwd.clone(),
        chaos_home: config.chaos_home.clone(),
        process_name: None,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        session_source: SessionSource::Exec,
        dynamic_tools,
        persist_extended_history: false,
        inherited_shell_snapshot: None,
    };
    let per_turn_config = Session::build_per_turn_config(&session_configuration);
    let model_info = ModelsManager::construct_model_info_offline_for_tests(
        session_configuration.collaboration_mode.model(),
        &per_turn_config,
    );
    let session_telemetry = session_telemetry(
        conversation_id,
        config.as_ref(),
        &model_info,
        session_configuration.session_source.clone(),
    );

    let state = SessionState::new(session_configuration.clone());
    let mcp_manager = Arc::new(McpManager::new());
    let skills_manager = Arc::new(SkillsManager::new(config.chaos_home.clone(), true));
    let network_approval = Arc::new(NetworkApprovalService::default());

    let file_watcher = Arc::new(FileWatcher::noop());
    let services = SessionServices {
        catalog: Arc::new(crate::catalog::CatalogSink::new(
            crate::catalog::Catalog::from_inventory(),
        )),
        mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::new_uninitialized(
            &config.permissions.approval_policy,
        ))),
        mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
        unified_exec_manager: UnifiedExecProcessManager::new(
            config.background_terminal_max_timeout,
        ),
        hooks: Hooks::new(HooksConfig {
            legacy_notify_argv: config.notify.clone(),
            ..HooksConfig::default()
        }),
        rollout: Mutex::new(None),
        user_shell: Arc::new(default_user_shell()),
        shell_snapshot_tx: watch::channel(None).0,

        exec_policy,
        auth_manager: Arc::clone(&auth_manager),
        session_telemetry: session_telemetry.clone(),
        models_manager: Arc::clone(&models_manager),
        tool_approvals: Mutex::new(ApprovalStore::default()),
        skills_manager,
        mcp_manager,
        file_watcher,
        agent_control,
        network_proxy: None,
        network_approval: Arc::clone(&network_approval),
        runtime_db: None,
        model_client: ModelClient::new(
            Some(Arc::clone(&auth_manager)),
            conversation_id,
            session_configuration.provider.clone(),
            session_configuration.session_source.clone(),
            session_configuration.approval_policy.value(),
            config.model_verbosity,
            true, // request compression always enabled
            Session::build_model_client_beta_features_header(config.as_ref()),
        ),
        hallucinate: None,
    };

    let skills_outcome = Arc::new(services.skills_manager.skills_for_config(&per_turn_config));
    let turn_context = Arc::new(Session::make_turn_context(
        Some(Arc::clone(&auth_manager)),
        &session_telemetry,
        session_configuration.provider.clone(),
        &session_configuration,
        per_turn_config,
        model_info,
        &models_manager,
        None,
        "turn_id".to_string(),
        skills_outcome,
    ));

    let session = Arc::new(Session {
        conversation_id,
        tx_event,
        agent_status: agent_status_tx,
        out_of_band_elicitation_paused: watch::channel(false).0,
        state: Mutex::new(state),
        features: config.features.clone(),
        pending_mcp_server_refresh_config: Mutex::new(None),

        active_turn: Mutex::new(None),

        services,
        next_internal_sub_id: AtomicU64::new(0),
    });

    (session, turn_context, rx_event)
}

// Like make_session_and_context, but returns Arc<Session> and the event receiver
// so tests can assert on emitted events.
pub(crate) async fn make_session_and_context_with_rx() -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
) {
    make_session_and_context_with_dynamic_tools_and_rx(Vec::new()).await
}

#[derive(Clone, Copy)]
struct NeverEndingTask {
    kind: TaskKind,
    listen_to_cancellation_token: bool,
}

impl SessionTask for NeverEndingTask {
    fn kind(&self) -> TaskKind {
        self.kind
    }

    fn span_name(&self) -> &'static str {
        "session_task.never_ending"
    }

    fn run(
        self: Arc<Self>,
        _session: Arc<SessionTaskContext>,
        _ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
        Box::pin(async move {
            if self.listen_to_cancellation_token {
                cancellation_token.cancelled().await;
                return None;
            }
            loop {
                sleep(Duration::from_secs(60)).await;
            }
        })
    }
}

async fn sample_rollout(
    session: &Session,
    _turn_context: &TurnContext,
) -> (Vec<RolloutItem>, Vec<ResponseItem>) {
    let mut rollout_items = Vec::new();
    let mut live_history = ContextManager::new();

    // Use the same turn_context source as record_initial_history so model_info (and thus
    // personality_spec) matches reconstruction.
    let reconstruction_turn = session.new_default_turn().await;
    let mut initial_context = session
        .build_initial_context(reconstruction_turn.as_ref())
        .await;
    // Ensure personality_spec is present when Personality is enabled, so expected matches
    // what reconstruction produces (build_initial_context may omit it when baked into model).
    if !initial_context.iter().any(|m| {
        matches!(m, ResponseItem::Message { role, content, .. }
        if role == "developer"
            && content.iter().any(|c| {
                matches!(c, ContentItem::InputText { text } if text.contains("<personality_spec>"))
            }))
    }) && let Some(p) = reconstruction_turn.personality
        && let Some(personality_message) = reconstruction_turn
            .model_info
            .model_messages
            .as_ref()
            .and_then(|m| m.get_personality_message(Some(p)).filter(|s| !s.is_empty()))
    {
        let msg = DeveloperInstructions::personality_spec_message(personality_message).into();
        let insert_at = initial_context
            .iter()
            .position(|m| matches!(m, ResponseItem::Message { role, .. } if role == "developer"))
            .map(|i| i + 1)
            .unwrap_or(0);
        initial_context.insert(insert_at, msg);
    }
    for item in &initial_context {
        rollout_items.push(RolloutItem::ResponseItem(item.clone()));
    }
    live_history.record_items(
        initial_context.iter(),
        reconstruction_turn.truncation_policy,
    );

    let user1 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "first user".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user1),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user1.clone()));

    let assistant1 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply one".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant1),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant1.clone()));

    let summary1 = "summary one";
    let snapshot1 = live_history
        .clone()
        .for_prompt(&reconstruction_turn.model_info.input_modalities);
    let user_messages1 = collect_user_messages(&snapshot1);
    let rebuilt1 = compact::build_compacted_history(Vec::new(), &user_messages1, summary1);
    live_history.replace(rebuilt1);
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: summary1.to_string(),
        replacement_history: None,
    }));

    let user2 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "second user".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user2),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user2.clone()));

    let assistant2 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply two".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant2),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant2.clone()));

    let summary2 = "summary two";
    let snapshot2 = live_history
        .clone()
        .for_prompt(&reconstruction_turn.model_info.input_modalities);
    let user_messages2 = collect_user_messages(&snapshot2);
    let rebuilt2 = compact::build_compacted_history(Vec::new(), &user_messages2, summary2);
    live_history.replace(rebuilt2);
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: summary2.to_string(),
        replacement_history: None,
    }));

    let user3 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "third user".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user3),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user3));

    let assistant3 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply three".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant3),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant3));

    (
        rollout_items,
        live_history.for_prompt(&reconstruction_turn.model_info.input_modalities),
    )
}

#[path = "chaos_tests/aborts.rs"]
mod aborts;
#[path = "chaos_tests/early_session.rs"]
mod early_session;
#[path = "chaos_tests/parser_network.rs"]
mod parser_network;
#[path = "chaos_tests/session_lifecycle.rs"]
mod session_lifecycle;
#[path = "chaos_tests/structured_output.rs"]
mod structured_output;
#[path = "chaos_tests/tools_rollout.rs"]
mod tools_rollout;
