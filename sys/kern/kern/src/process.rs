use crate::chaos::Chaos;
use crate::chaos::SteerInputError;
use crate::config::ConstraintResult;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use crate::features::Feature;
use crate::file_watcher::WatchRegistration;
use crate::minions::AgentStatus;
use crate::protocol::Event;
use crate::protocol::Op;
use crate::protocol::Submission;
use chaos_ipc::config_types::ApprovalsReviewer;
use chaos_ipc::config_types::Personality;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::W3cTraceContext;
use chaos_ipc::user_input::UserInput;
use std::path::PathBuf;
use tokio::sync::Mutex;
use tokio::sync::watch;

use crate::state_db::StateDbHandle;

#[derive(Clone, Debug)]
pub struct ProcessConfigSnapshot {
    pub model: String,
    pub model_provider_id: String,
    pub service_tier: Option<ServiceTier>,
    pub approval_policy: ApprovalPolicy,
    pub approvals_reviewer: ApprovalsReviewer,
    pub sandbox_policy: SandboxPolicy,
    pub cwd: PathBuf,
    pub ephemeral: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub personality: Option<Personality>,
    pub session_source: SessionSource,
}

pub struct Process {
    pub(crate) codex: Chaos,
    out_of_band_elicitation_count: Mutex<u64>,
    _watch_registration: WatchRegistration,
}

/// Conduit for the bidirectional stream of messages that compose a process
/// (formerly called a thread, and earlier a conversation) in ChaOS.
impl Process {
    pub(crate) fn new(codex: Chaos, watch_registration: WatchRegistration) -> Self {
        Self {
            codex,
            out_of_band_elicitation_count: Mutex::new(0),
            _watch_registration: watch_registration,
        }
    }

    pub async fn submit(&self, op: Op) -> ChaosResult<String> {
        self.codex.submit(op).await
    }

    pub async fn shutdown_and_wait(&self) -> ChaosResult<()> {
        self.codex.shutdown_and_wait().await
    }

    pub async fn submit_with_trace(
        &self,
        op: Op,
        trace: Option<W3cTraceContext>,
    ) -> ChaosResult<String> {
        self.codex.submit_with_trace(op, trace).await
    }

    pub async fn steer_input(
        &self,
        input: Vec<UserInput>,
        expected_turn_id: Option<&str>,
    ) -> Result<String, SteerInputError> {
        self.codex.steer_input(input, expected_turn_id).await
    }

    pub async fn set_app_server_client_name(
        &self,
        app_server_client_name: Option<String>,
    ) -> ConstraintResult<()> {
        self.codex
            .set_app_server_client_name(app_server_client_name)
            .await
    }

    /// Use sparingly: this is intended to be removed soon.
    pub async fn submit_with_id(&self, sub: Submission) -> ChaosResult<()> {
        self.codex.submit_with_id(sub).await
    }

    pub async fn next_event(&self) -> ChaosResult<Event> {
        self.codex.next_event().await
    }

    pub async fn agent_status(&self) -> AgentStatus {
        self.codex.agent_status().await
    }

    pub(crate) fn subscribe_status(&self) -> watch::Receiver<AgentStatus> {
        self.codex.agent_status.clone()
    }

    pub(crate) async fn total_token_usage(&self) -> Option<TokenUsage> {
        self.codex.session.total_token_usage().await
    }

    /// Records a user-role session-prefix message without creating a new user turn boundary.
    pub(crate) async fn inject_user_message_without_turn(&self, message: String) {
        let pending_item = ResponseInputItem::Message {
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: message }],
        };
        let pending_items = vec![pending_item];
        let Err(items_without_active_turn) = self
            .codex
            .session
            .inject_response_items(pending_items)
            .await
        else {
            return;
        };

        let turn_context = self.codex.session.new_default_turn().await;
        let items: Vec<ResponseItem> = items_without_active_turn
            .into_iter()
            .map(ResponseItem::from)
            .collect();
        self.codex
            .session
            .record_conversation_items(turn_context.as_ref(), &items)
            .await;
    }

    pub fn state_db(&self) -> Option<StateDbHandle> {
        self.codex.state_db()
    }

    pub async fn config_snapshot(&self) -> ProcessConfigSnapshot {
        self.codex.process_config_snapshot().await
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.codex.enabled(feature)
    }

    pub async fn increment_out_of_band_elicitation_count(&self) -> ChaosResult<u64> {
        let mut guard = self.out_of_band_elicitation_count.lock().await;
        let was_zero = *guard == 0;
        *guard = guard.checked_add(1).ok_or_else(|| {
            ChaosErr::Fatal("out-of-band elicitation count overflowed".to_string())
        })?;

        if was_zero {
            self.codex
                .session
                .set_out_of_band_elicitation_pause_state(/*paused*/ true);
        }

        Ok(*guard)
    }

    pub async fn decrement_out_of_band_elicitation_count(&self) -> ChaosResult<u64> {
        let mut guard = self.out_of_band_elicitation_count.lock().await;
        if *guard == 0 {
            return Err(ChaosErr::InvalidRequest(
                "out-of-band elicitation count is already zero".to_string(),
            ));
        }

        *guard -= 1;
        let now_zero = *guard == 0;
        if now_zero {
            self.codex
                .session
                .set_out_of_band_elicitation_pause_state(/*paused*/ false);
        }

        Ok(*guard)
    }
}
