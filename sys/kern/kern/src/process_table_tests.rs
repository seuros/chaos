use super::*;
use crate::chaos::make_session_and_context;
use crate::collaboration_modes::CollaborationModesConfig;
use crate::config::test_config;
use crate::models_manager::manager::RefreshStrategy;
use crate::test_support::{assistant_msg, user_msg};
use assert_matches::assert_matches;
use chaos_ipc::models::ReasoningItemReasoningSummary;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ModelsResponse;
use core_test_support::responses::mount_models_once;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use std::task::Poll;
use std::time::Duration;
use tempfile::tempdir;
use wiremock::MockServer;

#[test]
fn drops_from_last_user_only() {
    let items = [
        user_msg("u1"),
        assistant_msg("a1"),
        assistant_msg("a2"),
        user_msg("u2"),
        assistant_msg("a3"),
        ResponseItem::Reasoning {
            id: "r1".to_string(),
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "s".to_string(),
            }],
            content: None,
            encrypted_content: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            call_id: "c1".to_string(),
            name: "tool".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            provider_metadata: None,
        },
        assistant_msg("a4"),
    ];

    let initial: Vec<RolloutItem> = items
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect();
    let truncated = truncate_before_nth_user_message(InitialHistory::Forked(initial), 1);
    let got_items = truncated.get_rollout_items();
    let expected_items = vec![
        RolloutItem::ResponseItem(items[0].clone()),
        RolloutItem::ResponseItem(items[1].clone()),
        RolloutItem::ResponseItem(items[2].clone()),
    ];
    assert_eq!(
        serde_json::to_value(&got_items).unwrap(),
        serde_json::to_value(&expected_items).unwrap()
    );

    let initial2: Vec<RolloutItem> = items
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect();
    let truncated2 = truncate_before_nth_user_message(InitialHistory::Forked(initial2), 2);
    assert_matches!(truncated2, InitialHistory::New);
}

#[tokio::test]
async fn ignores_session_prefix_messages_when_truncating() {
    let (session, turn_context) = make_session_and_context().await;
    let mut items = session.build_initial_context(&turn_context).await;
    items.push(user_msg("feature request"));
    items.push(assistant_msg("ack"));
    items.push(user_msg("second question"));
    items.push(assistant_msg("answer"));

    let rollout_items: Vec<RolloutItem> = items
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect();

    let truncated = truncate_before_nth_user_message(InitialHistory::Forked(rollout_items), 1);
    let got_items = truncated.get_rollout_items();

    let expected: Vec<RolloutItem> = vec![
        RolloutItem::ResponseItem(items[0].clone()),
        RolloutItem::ResponseItem(items[1].clone()),
        RolloutItem::ResponseItem(items[2].clone()),
        RolloutItem::ResponseItem(items[3].clone()),
    ];

    assert_eq!(
        serde_json::to_value(&got_items).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}

#[derive(Clone)]
struct ShutdownFixture {
    submissions: async_channel::Sender<Op>,
    termination: futures::future::Shared<futures::future::BoxFuture<'static, ()>>,
}

impl ShutdownFixture {
    async fn shutdown(self) -> ChaosResult<()> {
        self.submissions.send(Op::Shutdown).await.unwrap();
        self.termination.await;
        Ok(())
    }
}

async fn insert_shutdown_fixture(
    processes: &RwLock<HashMap<ProcessId, ShutdownFixture>>,
    termination: impl std::future::Future<Output = ()> + Send + 'static,
) -> (ProcessId, async_channel::Receiver<Op>) {
    let process_id = ProcessId::new();
    let (submissions, receiver) = async_channel::bounded(1);
    processes.write().await.insert(
        process_id,
        ShutdownFixture {
            submissions,
            termination: termination.boxed().shared(),
        },
    );
    (process_id, receiver)
}

#[tokio::test(start_paused = true)]
async fn shutdown_all_threads_bounded_submits_shutdown_to_every_thread() {
    let processes = RwLock::new(HashMap::new());
    let (done_1, terminated_1) = tokio::sync::oneshot::channel();
    let (done_2, terminated_2) = tokio::sync::oneshot::channel();
    let (thread_1, submissions_1) = insert_shutdown_fixture(&processes, async {
        terminated_1.await.expect("first termination");
    })
    .await;
    let (thread_2, submissions_2) = insert_shutdown_fixture(&processes, async {
        terminated_2.await.expect("second termination");
    })
    .await;

    let mut shutdown = std::pin::pin!(shutdown_processes_bounded(
        &processes,
        Duration::from_secs(10),
        ShutdownFixture::shutdown
    ));
    assert!(futures::poll!(&mut shutdown).is_pending());
    // Every submission must happen before either process acknowledges shutdown.
    for submissions in [&submissions_1, &submissions_2] {
        assert_matches!(
            submissions.try_recv().expect("shutdown submission"),
            Op::Shutdown
        );
        assert!(submissions.is_empty());
    }
    done_1.send(()).expect("complete first process");
    assert!(futures::poll!(&mut shutdown).is_pending());
    done_2.send(()).expect("complete second process");
    let Poll::Ready(report) = futures::poll!(&mut shutdown) else {
        panic!("shutdown should complete after both acknowledgements");
    };

    let mut expected_completed = vec![thread_1, thread_2];
    expected_completed.sort_by_key(std::string::ToString::to_string);
    assert_eq!(report.completed, expected_completed);
    assert!(report.submit_failed.is_empty());
    assert!(report.timed_out.is_empty());
    assert!(processes.read().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn shutdown_all_threads_bounded_retains_unfinished_processes_for_retry() {
    let processes = RwLock::new(HashMap::new());
    let (completed, completed_submissions) =
        insert_shutdown_fixture(&processes, std::future::ready(())).await;
    let (pending, pending_submissions) =
        insert_shutdown_fixture(&processes, std::future::pending()).await;
    let (blocked, blocked_submissions) =
        insert_shutdown_fixture(&processes, std::future::ready(())).await;
    processes
        .read()
        .await
        .get(&blocked)
        .unwrap()
        .submissions
        .try_send(Op::Interrupt)
        .expect("fill submission channel");

    let timeout = Duration::from_millis(25);
    let started = tokio::time::Instant::now();
    let mut shutdown = std::pin::pin!(shutdown_processes_bounded(
        &processes,
        timeout,
        ShutdownFixture::shutdown
    ));
    assert!(futures::poll!(&mut shutdown).is_pending());
    for submissions in [&completed_submissions, &pending_submissions] {
        assert_matches!(submissions.try_recv().unwrap(), Op::Shutdown);
    }
    tokio::time::advance(timeout).await;
    let Poll::Ready(report) = futures::poll!(&mut shutdown) else {
        panic!("all pending shutdowns must share the bounded deadline");
    };
    let mut unfinished = vec![pending, blocked];
    unfinished.sort_by_key(std::string::ToString::to_string);
    assert_eq!(report.completed, vec![completed]);
    assert!(report.submit_failed.is_empty());
    assert_eq!(report.timed_out, unfinished);
    assert_eq!(started.elapsed(), timeout);
    let mut tracked = processes.read().await.keys().copied().collect::<Vec<_>>();
    tracked.sort_by_key(std::string::ToString::to_string);
    assert_eq!(tracked, unfinished);
    assert_matches!(blocked_submissions.try_recv().unwrap(), Op::Interrupt);
    assert!(
        blocked_submissions.is_empty(),
        "timed-out submission was dropped"
    );

    // Releasing capacity permits a later retry; the still-running process stays tracked.
    let report = shutdown_processes_bounded(&processes, timeout, ShutdownFixture::shutdown).await;
    assert_eq!(report.completed, vec![blocked]);
    assert_eq!(report.timed_out, vec![pending]);
    assert_eq!(
        processes.read().await.keys().copied().collect::<Vec<_>>(),
        vec![pending]
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_all_threads_bounded_reports_submission_errors_without_removing_them() {
    let completed = ProcessId::new();
    let failed = ProcessId::new();
    let processes = RwLock::new(HashMap::from([(completed, true), (failed, false)]));
    let report = shutdown_processes_bounded(&processes, Duration::from_secs(10), |succeeds| {
        std::future::ready(if succeeds {
            Ok(())
        } else {
            Err(ChaosErr::InternalAgentDied)
        })
    })
    .await;
    assert_eq!(report.completed, vec![completed]);
    assert_eq!(report.submit_failed, vec![failed]);
    assert!(report.timed_out.is_empty());
    assert_eq!(*processes.read().await, HashMap::from([(failed, false)]));
}

#[tokio::test]
async fn new_uses_configured_openai_provider_for_model_refresh() {
    let server = MockServer::start().await;
    let models_mock = mount_models_once(&server, ModelsResponse { models: vec![] }).await;

    let temp_dir = tempdir().expect("tempdir");
    let mut config = test_config();
    config.chaos_home = temp_dir.path().join("chaos-home");
    config.cwd = config.chaos_home.clone();
    std::fs::create_dir_all(&config.chaos_home).expect("create chaos home");
    config.model_catalog = None;
    config
        .model_providers
        .get_mut("openai")
        .expect("openai provider should exist")
        .base_url = Some(server.uri());
    // ProcessTable now uses model_provider (the active provider) for
    // model discovery, not the hardcoded "openai" entry.
    config.model_provider.base_url = Some(server.uri());

    let auth_manager =
        AuthManager::from_auth_for_testing(ChaosAuth::create_dummy_chatgpt_auth_for_testing());
    let manager = ProcessTable::new(
        &config,
        auth_manager,
        SessionSource::Exec,
        CollaborationModesConfig::default(),
    );

    let _ = manager.list_models(RefreshStrategy::Online).await;
    assert_eq!(models_mock.requests().len(), 1);
}
