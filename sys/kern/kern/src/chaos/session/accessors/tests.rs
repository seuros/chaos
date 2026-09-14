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
        handle: AbortOnDropHandle::new(tokio::spawn(async {})),
        turn_context: Arc::new(turn),
        unrecorded_input: Arc::default(),
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
