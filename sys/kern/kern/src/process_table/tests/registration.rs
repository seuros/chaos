use super::*;
use pretty_assertions::assert_eq;

async fn registration_fixture(
    table: &ProcessTable,
) -> (
    Chaos,
    ProcessId,
    crate::file_watcher::WatchRegistration,
    async_channel::Receiver<chaos_ipc::protocol::Submission>,
) {
    // Offline native-session fixture: no model process or provider request.
    let (session, turn) = make_session_and_context().await;
    let process_id = session.conversation_id;
    let config = &turn.config;
    let configured = SessionConfiguredEvent {
        session_id: process_id,
        forked_from_id: None,
        process_name: None,
        model: "fixture".into(),
        model_provider_id: config.model_provider_id.clone(),
        service_tier: config.service_tier,
        approval_policy: config.permissions.approval_policy.value(),
        approvals_reviewer: config.approvals_reviewer,
        vfs_policy: config.permissions.vfs_policy.clone(),
        socket_policy: config.permissions.socket_policy,
        cwd: config.cwd.clone(),
        reasoning_effort: None,
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        network_proxy: None,
    };
    let (events, rx_event) = async_channel::bounded(1);
    events
        .send(Event {
            id: INITIAL_SUBMIT_ID.into(),
            msg: EventMsg::SessionConfigured(configured),
        })
        .await
        .unwrap();
    let (tx_sub, rx_sub) = async_channel::bounded(1);
    let chaos = Chaos {
        tracks_submissions: true,
        tx_sub,
        rx_event,
        agent_status: tokio::sync::watch::channel(crate::minions::AgentStatus::PendingInit).1,
        session: Arc::new(session),
        session_loop_termination: crate::chaos::completed_session_loop_termination(),
    };
    let watch = table.state.file_watcher.register_config(config);
    (chaos, process_id, watch, rx_sub)
}

fn table() -> ProcessTable {
    ProcessTable::with_models_provider_for_tests(
        ChaosAuth::from_api_key("unused"),
        crate::model_provider_info::create_oss_provider_with_base_url(
            "http://127.0.0.1:1",
            crate::WireApi::Responses,
        ),
    )
}

#[tokio::test]
async fn cancelled_registration_waiting_for_history_never_publishes_process() {
    let table = table();
    let (chaos, id, watch, submissions) = registration_fixture(&table).await;
    let mut histories = table.state.closed_process_histories.write().await;
    histories.insert(
        id,
        vec![RolloutItem::ResponseItem(user_msg("retained history"))],
    );
    let mut registration = Box::pin(table.state.finalize_process_spawn(chaos, id, watch));
    assert!(futures::poll!(registration.as_mut()).is_pending());
    assert!(
        !table.state.processes.read().await.contains_key(&id),
        "blocked registration must remain unpublished"
    );
    drop(registration);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), submissions.recv())
            .await
            .expect("submission sender must close")
            .is_err(),
        "cancellation must drop the unregistered process's submission sender"
    );
    assert!(
        histories.contains_key(&id),
        "cancelled handoff preserves history"
    );
}

#[tokio::test]
async fn successful_registration_publishes_process_and_removes_stashed_history_together() {
    let table = table();
    let (chaos, id, watch, submissions) = registration_fixture(&table).await;
    table.state.closed_process_histories.write().await.insert(
        id,
        vec![RolloutItem::ResponseItem(user_msg("retained history"))],
    );
    let registered = table
        .state
        .finalize_process_spawn(chaos, id, watch)
        .await
        .expect("registration succeeds");
    assert_eq!(registered.process_id(), id);
    assert!(table.state.processes.read().await.contains_key(&id));
    assert!(
        !table
            .state
            .closed_process_histories
            .read()
            .await
            .contains_key(&id)
    );
    let process = table.state.remove_process(&id).await;
    drop(registered);
    drop(process);
    assert!(submissions.recv().await.is_err());
}
