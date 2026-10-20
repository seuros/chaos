use super::*;
use crate::chaos::make_session_and_context;
use crate::config::test_config;
use crate::test_support::{assistant_msg, user_msg};
use chaos_ipc::models::ReasoningItemReasoningSummary;
use chaos_ipc::models::ResponseItem;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use std::assert_matches;
use std::task::Poll;
use std::time::Duration;
use tempfile::tempdir;

mod discovery;
mod registration;

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

async fn insert_live_descendant(
    table: &ProcessTable,
    parent: ProcessId,
) -> (
    ProcessId,
    Arc<crate::chaos::Session>,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let (session, turn) = make_session_and_context().await;
    session
        .set_session_source_for_tests(SessionSource::SubAgent(
            chaos_ipc::protocol::SubAgentSource::ProcessSpawn {
                parent_process_id: parent,
                depth: 1,
                agent_nickname: None,
                agent_role: None,
            },
        ))
        .await;
    session.services.internal_task_store.set_active(true).await;
    let session = Arc::new(session);
    let id = session.conversation_id;
    let (tx_sub, rx_sub) = async_channel::bounded::<chaos_ipc::protocol::Submission>(8);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let task = tokio::spawn({
        let session = session.clone();
        let entered = entered.clone();
        let release = release.clone();
        async move {
            assert!(matches!(rx_sub.recv().await.unwrap().op, Op::Shutdown));
            entered.notify_one();
            release.notified().await;
            session.services.internal_task_store.set_active(false).await;
        }
    });
    let chaos = Chaos {
        tracks_submissions: true,
        tx_sub,
        rx_event: async_channel::bounded(1).1,
        // Completed foreground status must not conceal ongoing background work.
        agent_status: tokio::sync::watch::channel(crate::minions::AgentStatus::Completed(None)).1,
        session: session.clone(),
        session_loop_termination: crate::chaos::session_loop_termination_from_handle(task),
    };
    table.state.processes.write().await.insert(
        id,
        Arc::new(Process::new(
            chaos,
            table.state.file_watcher.register_config(&turn.config),
        )),
    );
    (id, session, entered, release)
}

#[tokio::test]
async fn descendant_containment_retains_ownership_across_cancellation_and_late_grandchildren() {
    let table = Arc::new(ProcessTable::with_models_provider_for_tests(
        ChaosAuth::from_api_key("unused"),
        crate::model_provider_info::create_oss_provider_with_base_url(
            "http://127.0.0.1:1",
            crate::WireApi::Responses,
        ),
    ));
    let (root, _) = table
        .insert_status_only_process_for_tests(crate::minions::AgentStatus::Completed(None))
        .await;
    let (child, _, entered, release) = insert_live_descendant(&table, root).await;
    let (unrelated, unrelated_session, _, unrelated_release) =
        insert_live_descendant(&table, ProcessId::new()).await;
    assert!(!table.process_tree_is_quiescent(root).await.unwrap());
    let first = tokio::spawn({
        let table = table.clone();
        async move { table.shutdown_descendants_and_wait(root).await }
    });
    entered.notified().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(table.get_process(child).await.is_ok());
    assert!(!table.process_tree_is_quiescent(root).await.unwrap());
    // This admission was in flight when root containment started. The child
    // must remain tracked so the next pass can still discover its descendants.
    let (grandchild, _, grandchild_entered, grandchild_release) =
        insert_live_descendant(&table, child).await;
    release.notify_one();
    let retry = tokio::spawn({
        let table = table.clone();
        async move { table.shutdown_descendants_and_wait(root).await }
    });
    grandchild_entered.notified().await;
    assert!(table.get_process(child).await.is_ok());
    assert!(table.get_process(grandchild).await.is_ok());
    assert!(!table.process_tree_is_quiescent(root).await.unwrap());
    grandchild_release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), retry)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(table.get_process(child).await.is_err());
    assert!(table.get_process(grandchild).await.is_err());
    assert!(table.process_tree_is_quiescent(root).await.unwrap());
    assert!(table.get_process(unrelated).await.is_ok());
    assert!(
        !unrelated_session
            .services
            .internal_task_store
            .subscribe()
            .borrow()
            .is_quiescent()
    );
    unrelated_release.notify_one();
    table
        .get_process(unrelated)
        .await
        .unwrap()
        .shutdown_and_wait()
        .await
        .unwrap();
}

#[tokio::test]
async fn abandoned_descendant_boot_blocks_quiescence_and_cannot_claim_containment() {
    let table = ProcessTable::with_models_provider_for_tests(
        ChaosAuth::from_api_key("unused"),
        crate::model_provider_info::create_oss_provider_with_base_url(
            "http://127.0.0.1:1",
            crate::WireApi::Responses,
        ),
    );
    let (root, _) = table
        .insert_status_only_process_for_tests(crate::minions::AgentStatus::Completed(None))
        .await;
    let (other, _) = table
        .insert_status_only_process_for_tests(crate::minions::AgentStatus::Completed(None))
        .await;
    let pending = table.state.pending_admission(root).unwrap();
    assert!(!table.process_tree_is_quiescent(root).await.unwrap());
    assert!(table.process_tree_is_quiescent(other).await.unwrap());
    // The future might have been cancelled during MCP/native initialization.
    // Dropping its local guard is not evidence of containment.
    drop(pending);
    assert!(!table.process_tree_is_quiescent(root).await.unwrap());
    assert!(table.shutdown_descendants_and_wait(root).await.is_err());
    assert!(table.get_process(root).await.is_ok());
    assert_eq!(table.state.pending_descendants.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn quiescence_snapshot_cannot_miss_a_child_inserted_before_its_pending_entry_clears() {
    let table = ProcessTable::with_models_provider_for_tests(
        ChaosAuth::from_api_key("unused"),
        crate::model_provider_info::create_oss_provider_with_base_url(
            "http://127.0.0.1:1",
            crate::WireApi::Responses,
        ),
    );
    let (root, _) = table
        .insert_status_only_process_for_tests(crate::minions::AgentStatus::Completed(None))
        .await;
    let root_process = table.get_process(root).await.unwrap();
    // Block after registry snapshot, while reading the root's configuration.
    let state = root_process.chaos.session.state.lock().await;
    let mut check = Box::pin(table.process_tree_is_quiescent(root));
    assert!(futures::poll!(&mut check).is_pending());
    let pending = table.state.pending_admission(root).unwrap();
    let (child, _, _, release) = insert_live_descendant(&table, root).await;
    table.state.track_descendant(root, child).unwrap();
    pending.registered().unwrap();
    drop(state);
    // The snapshot contains only the idle root and pending is already empty.
    // Admission generation, not a lucky second registry read, must reject it.
    assert!(!check.await.unwrap());
    assert!(!table.process_tree_is_quiescent(root).await.unwrap());
    release.notify_one();
    table.shutdown_descendants_and_wait(root).await.unwrap();
    assert!(table.process_tree_is_quiescent(root).await.unwrap());
}
