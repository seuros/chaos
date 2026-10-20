use super::*;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_kern::journal_client_for_mounted_backend;
use core_test_support::responses::{ev_function_call, ev_reasoning_item_added};

fn params(session: &Value, cwd: &std::path::Path) -> Value {
    json!({"sessionId":session,"cwd":cwd,"mcpServers":[]})
}

fn texts(updates: &[Value], kind: &str) -> String {
    updates
        .iter()
        .filter(|update| update["params"]["update"]["sessionUpdate"] == kind)
        .filter_map(|update| update["params"]["update"]["content"]["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_load_and_resume_keep_identity_controls_history_and_native_ownership() {
    let mut fixture = Fixture::new().await;
    fixture.config.model_catalog = Some(chaos_kern::test_support::test_models_response(&[
        "mock-model",
        "mock-second",
    ]));
    let file = fixture.cwd.path().join("read-before-restart.txt");
    std::fs::write(&file, "history-visible-file-marker\n").unwrap();
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![
            sse(vec![
                ev_response_created("history-r1"),
                ev_reasoning_item_added("private-reasoning-marker", &[]),
                ev_function_call(
                    "history-read",
                    "read_file",
                    &json!({"file_path":file}).to_string(),
                ),
                ev_completed("history-r1"),
            ]),
            response("history-r2", "public response before restart"),
            response("history-r3", "continued after restoration"),
        ],
    )
    .await;
    let (mut first, session) = fixture.peer().await;
    assert_eq!(
        first.initialized["result"]["agentCapabilities"]["loadSession"],
        true
    );
    assert!(
        first.initialized["result"]["agentCapabilities"]["sessionCapabilities"]["list"].is_object()
    );
    let (selected, _) = first
        .request(
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"mock-second"}),
        )
        .await;
    assert!(selected["error"].is_null(), "{selected}");
    let (selected, _) = first
        .request(
            "session/set_mode",
            json!({"sessionId":session,"modeId":"plan"}),
        )
        .await;
    assert!(selected["error"].is_null(), "{selected}");
    let turn = first.prompt(&session, "public user before restart").await;
    first.finish(&session, turn, "end_turn").await;
    assert_eq!(requests.requests().len(), 2);
    // A second connection cannot seize an active native writer.
    let mut contender = Peer::connect(fixture.config.clone()).await;
    let (busy, _) = contender
        .request("session/resume", params(&session, fixture.cwd.path()))
        .await;
    assert!(busy["error"].is_object(), "{busy}");
    contender.close().await;
    first.close().await;

    let mut second = Peer::connect(fixture.config.clone()).await;
    let (listed, _) = second.request("session/list", json!({})).await;
    let rows = listed["result"]["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["sessionId"], session);
    let (loaded, updates) = second
        .request("session/load", params(&session, fixture.cwd.path()))
        .await;
    assert!(loaded["error"].is_null(), "{loaded}");
    assert_eq!(
        texts(&updates, "user_message_chunk"),
        "public user before restart"
    );
    assert_eq!(
        texts(&updates, "agent_message_chunk"),
        "public response before restart"
    );
    assert!(
        !serde_json::to_string(&updates)
            .unwrap()
            .contains("private-reasoning-marker")
    );
    assert!(
        !serde_json::to_string(&updates)
            .unwrap()
            .contains("developer")
    );
    assert!(
        updates
            .iter()
            .any(|update| update["params"]["update"]["toolCallId"] == "history-read")
    );
    assert_eq!(loaded["result"]["modes"]["currentModeId"], "plan");
    assert!(
        loaded["result"]["configOptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["id"] == "model" && option["currentValue"] == "mock-second")
    );
    assert_eq!(
        requests.requests().len(),
        2,
        "loading must not call the provider"
    );
    let (duplicate, _) = second
        .request("session/load", params(&session, fixture.cwd.path()))
        .await;
    assert!(duplicate["error"].is_object());
    second.close().await;

    let mut third = Peer::connect(fixture.config.clone()).await;
    let (resumed, updates) = third
        .request("session/resume", params(&session, fixture.cwd.path()))
        .await;
    assert!(resumed["error"].is_null(), "{resumed}");
    assert!(
        updates
            .iter()
            .all(|frame| frame["params"]["update"]["sessionUpdate"] == "available_commands_update"),
        "resume must not replay the transcript"
    );
    let turn = third.prompt(&session, "fresh input after restart").await;
    third.finish(&session, turn, "end_turn").await;
    let body = requests.requests()[2].body_json();
    assert_eq!(body["model"], "mock-second");
    assert!(body.to_string().contains("public user before restart"));
    assert!(body.to_string().contains("public response before restart"));
    third.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restoration_rejects_foreign_source_provider_workspace_and_bad_cursors() {
    let fixture = Fixture::new().await;
    let (first, session) = fixture.peer().await;
    first.close().await;
    let journal = journal_client_for_mounted_backend().await.unwrap();
    let id = ProcessId::from_string(session.as_str().unwrap()).unwrap();
    let record = journal.get_process(id).await.unwrap().unwrap();
    let foreign = ProcessId::new();
    journal
        .create_process(chaos_journald::CreateProcessInput {
            process_id: foreign,
            parent: None,
            source: SessionSource::Api,
            cwd: record.cwd,
            created_at: record.created_at,
            title: None,
            model_provider: Some(fixture.config.model_provider_id.clone()),
            cli_version: None,
        })
        .await
        .unwrap();
    let mut peer = Peer::connect(fixture.config.clone()).await;
    let (bad, _) = peer
        .request(
            "session/load",
            params(&json!(foreign.to_string()), fixture.cwd.path()),
        )
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    let other_cwd = TempDir::new().unwrap();
    let (bad, _) = peer
        .request("session/load", params(&session, other_cwd.path()))
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    let (bad, _) = peer
        .request("session/list", json!({"cursor":"malformed"}))
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    peer.close().await;
    let mut other = fixture.config.clone();
    other.model_provider_id = "another-acp-provider".into();
    let mut peer = Peer::connect(other).await;
    let (bad, _) = peer
        .request("session/resume", params(&session, fixture.cwd.path()))
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    let (empty, _) = peer.request("session/list", json!({})).await;
    assert_eq!(empty["result"]["sessions"], json!([]), "{empty}");
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_metadata_exists_before_first_prompt_and_paging_is_bounded() {
    let fixture = Fixture::new().await;
    let (mut peer, first) = fixture.peer().await;
    for _ in 0..3 {
        let (opened, _) = peer
            .request(
                "session/new",
                json!({"cwd":fixture.cwd.path(),"mcpServers":[]}),
            )
            .await;
        assert!(opened["error"].is_null(), "{opened}");
    }
    let cwd = std::fs::canonicalize(fixture.cwd.path()).unwrap();
    let first_page = crate::history::list_page(&fixture.config, Some(&cwd), None, 2)
        .await
        .unwrap();
    assert_eq!(first_page.sessions.len(), 2);
    let second_page =
        crate::history::list_page(&fixture.config, Some(&cwd), first_page.next_cursor, 2)
            .await
            .unwrap();
    assert_eq!(second_page.sessions.len(), 2);
    assert!(first_page.sessions.iter().all(|first| {
        second_page
            .sessions
            .iter()
            .all(|second| first.session_id != second.session_id)
    }));
    peer.close().await;
    // The generic resume guard stays strict; only verified ACP restoration can
    // accept a never-prompted, durably checkpointed conversation.
    let id = ProcessId::from_string(first.as_str().unwrap()).unwrap();
    assert!(
        chaos_kern::RolloutRecorder::get_rollout_history_for_process(id)
            .await
            .is_err()
    );
    let mut peer = Peer::connect(fixture.config.clone()).await;
    let (restored, updates) = peer
        .request("session/load", params(&first, fixture.cwd.path()))
        .await;
    assert!(restored["error"].is_null(), "{restored}");
    super::commands::assert_only_commands(&updates);
    peer.close().await;
}

async fn append_saved(id: ProcessId, item: chaos_ipc::protocol::RolloutItem) {
    let journal = journal_client_for_mounted_backend().await.unwrap();
    let loaded = journal.load_journal(id).await.unwrap();
    let lease = journal
        .acquire_lease(id, format!("acp-test-{id}"), 30_000)
        .await
        .unwrap();
    let recorded_at = journal.get_process(id).await.unwrap().unwrap().updated_at;
    journal
        .append_batch(chaos_journald::AppendBatchInput {
            process_id: id,
            owner_id: lease.owner_id.clone(),
            lease_token: lease.lease_token.clone(),
            expected_next_seq: loaded.next_seq,
            items: vec![chaos_journald::JournalEntry {
                seq: loaded.next_seq,
                recorded_at,
                item,
            }],
        })
        .await
        .unwrap();
    journal
        .release_lease(id, lease.owner_id, lease.lease_token)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unfinished_foreground_turn_cannot_be_reexecuted_on_restore() {
    let fixture = Fixture::new().await;
    let (peer, session) = fixture.peer().await;
    peer.close().await;
    let id = ProcessId::from_string(session.as_str().unwrap()).unwrap();
    append_saved(
        id,
        chaos_ipc::protocol::RolloutItem::EventMsg(chaos_ipc::protocol::EventMsg::TurnStarted(
            chaos_ipc::protocol::TurnStartedEvent {
                turn_id: "interrupted-before-outcome".into(),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            },
        )),
    )
    .await;
    let mut peer = Peer::connect(fixture.config.clone()).await;
    for method in ["session/load", "session/resume"] {
        let (failed, updates) = peer
            .request(method, params(&session, fixture.cwd.path()))
            .await;
        assert!(failed["error"].is_object(), "{failed}");
        assert!(updates.is_empty());
    }
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_does_not_recover_previous_goal_or_background_continuations() {
    use chaos_ipc::protocol::{GoalCheckpointItem, RolloutItem};
    let fixture = Fixture::new().await;
    let (peer, session) = fixture.peer().await;
    peer.close().await;
    let id = ProcessId::from_string(session.as_str().unwrap()).unwrap();
    append_saved(
        id,
        RolloutItem::GoalCheckpoint(GoalCheckpointItem {
            conversation_id: id,
            server: "not-a-current-authorized-goal-server".into(),
            endpoint: "must-not-be-called".into(),
            version: 1,
            snapshot: json!({"private":"not public transcript"}),
            verdict: None,
        }),
    )
    .await;
    append_saved(
        id,
        RolloutItem::BackgroundTask(
            chaos_ipc::background_tasks::TaskJournalEvent::ContinuationStarted {
                turn_id: "do-not-recover-this-executor".into(),
            },
        ),
    )
    .await;
    let mut peer = Peer::connect(fixture.config.clone()).await;
    let (loaded, updates) = peer
        .request("session/load", params(&session, fixture.cwd.path()))
        .await;
    assert!(loaded["error"].is_null(), "{loaded}");
    super::commands::assert_only_commands(&updates);
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_new_never_claims_durability_when_the_native_journal_is_disabled() {
    let mut fixture = Fixture::new().await;
    fixture.config.ephemeral = true;
    let mut peer = Peer::connect(fixture.config.clone()).await;
    let (failed, _) = peer
        .request(
            "session/new",
            json!({"cwd":fixture.cwd.path(),"mcpServers":[]}),
        )
        .await;
    assert!(failed["error"].is_object(), "{failed}");
    assert!(failed["result"].is_null());
    peer.close().await;
}
