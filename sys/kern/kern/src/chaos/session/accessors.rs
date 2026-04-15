use std::collections::HashMap;
use std::sync::Arc;

use async_channel::Sender;
use chaos_dtrace::Hooks;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::protocol::Event;

use crate::config::ManagedFeatures;
use crate::features::Feature;

use super::Session;

impl Session {
    pub(crate) fn get_tx_event(&self) -> Sender<Event> {
        self.tx_event.clone()
    }

    pub(crate) fn runtime_db(&self) -> Option<crate::runtime_db::RuntimeDbHandle> {
        self.services.runtime_db.clone()
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.features.enabled(feature)
    }

    pub(crate) fn features(&self) -> ManagedFeatures {
        self.features.clone()
    }

    pub(crate) async fn collaboration_mode(&self) -> CollaborationMode {
        let state = self.state.lock().await;
        state.session_configuration.collaboration_mode.clone()
    }

    pub(crate) fn hooks(&self) -> &Hooks {
        &self.services.hooks
    }

    pub(crate) fn user_shell(&self) -> Arc<crate::shell::Shell> {
        Arc::clone(&self.services.user_shell)
    }

    pub(crate) async fn take_pending_session_start_source(
        &self,
    ) -> Option<chaos_dtrace::SessionStartSource> {
        let mut state = self.state.lock().await;
        state.take_pending_session_start_source()
    }

    pub async fn dependency_env(&self) -> HashMap<String, String> {
        let state = self.state.lock().await;
        state.dependency_env()
    }

    /// Effective config to use for child spawns.
    ///
    /// Prefer the currently active turn's config when a task is running so
    /// child agents inherit live turn overrides (approval/sandbox/cwd/model
    /// provider/etc.) rather than the session's init-time config snapshot.
    /// When no turn is active, rebuild from the latest `SessionConfiguration`
    /// so session-level updates are still reflected.
    pub(crate) async fn effective_config_for_spawn(&self) -> crate::config::Config {
        {
            let active_turn = self.active_turn.lock().await;
            if let Some(active_turn) = active_turn.as_ref()
                && let Some((_, task)) = active_turn.tasks.first()
            {
                return (*task.turn_context.config).clone();
            }
        }

        let state = self.state.lock().await;
        Self::build_per_turn_config(&state.session_configuration)
    }

    /// The session's own `SessionSource`. Used to derive a
    /// `SubAgentSource::ProcessSpawn` for child spawns so depth
    /// and parent linkage are tagged correctly.
    pub(crate) async fn session_source(&self) -> chaos_ipc::protocol::SessionSource {
        let state = self.state.lock().await;
        state.session_configuration.session_source.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::AbortOnDropHandle;

    use crate::chaos::TurnContext;
    use crate::chaos::make_session_and_context;
    use crate::protocol::ApprovalPolicy;
    use crate::state::ActiveTurn;
    use crate::state::RunningTask;
    use crate::state::TaskKind;
    use crate::tasks::SessionTask;
    use crate::tasks::SessionTaskContext;
    use chaos_ipc::user_input::UserInput;

    struct NoopTask;

    impl SessionTask for NoopTask {
        fn kind(&self) -> TaskKind {
            TaskKind::Regular
        }

        fn span_name(&self) -> &'static str {
            "test.noop"
        }

        fn run(
            self: Arc<Self>,
            _session: Arc<SessionTaskContext>,
            _ctx: Arc<TurnContext>,
            _input: Vec<UserInput>,
            _cancellation_token: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
            Box::pin(async { None })
        }
    }

    #[tokio::test]
    async fn effective_config_for_spawn_uses_current_session_configuration_without_active_turn() {
        let (session, _turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let expected_cwd = temp_dir.path().to_path_buf();

        {
            let mut state = session.state.lock().await;
            state.session_configuration.cwd = expected_cwd.clone();
            state
                .session_configuration
                .approval_policy
                .set(ApprovalPolicy::Interactive)
                .expect("approval policy set");
        }

        let got = session.effective_config_for_spawn().await;

        assert_eq!(got.cwd, expected_cwd);
        assert_eq!(
            got.permissions.approval_policy.value(),
            ApprovalPolicy::Interactive
        );
    }

    #[tokio::test]
    async fn effective_config_for_spawn_prefers_active_turn_config() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let expected_cwd = temp_dir.path().to_path_buf();

        let mut live_config = (*turn.config).clone();
        live_config.cwd = expected_cwd.clone();
        live_config
            .permissions
            .approval_policy
            .set(ApprovalPolicy::Interactive)
            .expect("approval policy set");
        turn.cwd = expected_cwd.clone();
        turn.approval_policy
            .set(ApprovalPolicy::Interactive)
            .expect("approval policy set");
        turn.config = Arc::new(live_config);

        let mut active_turn = ActiveTurn::default();
        active_turn.add_task(RunningTask {
            done: Arc::new(Notify::new()),
            kind: TaskKind::Regular,
            task: Arc::new(NoopTask),
            cancellation_token: CancellationToken::new(),
            handle: Arc::new(AbortOnDropHandle::new(tokio::spawn(async {}))),
            turn_context: Arc::new(turn),
            _timer: None,
        });
        *session.active_turn.lock().await = Some(active_turn);

        let got = session.effective_config_for_spawn().await;

        assert_eq!(got.cwd, expected_cwd);
        assert_eq!(
            got.permissions.approval_policy.value(),
            ApprovalPolicy::Interactive
        );
    }
}
