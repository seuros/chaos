//! Commit barriers, eviction, takeover and reconnect, on both storage engines.
use super::*;
use rama::http::body::util::BodyExt;

#[tokio::test]
async fn lost_writer_cannot_publish_a_terminal_event_or_release_capacity() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Execute(_, _)
    ));
    fixture.db.expire(&fixture.session.id).await;
    supervisor.events.send(complete(&id)).unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    status(&fixture.session, SessionStatus::Unknown).await;
    assert_eq!(fixture.session.snapshot().last_event_id, 0);
    assert!(fixture.session.turn(&id).unwrap().result.is_none());
    assert!(
        fixture
            .session
            .read_events(0)
            .await
            .unwrap()
            .events
            .is_empty()
    );
    assert_eq!(fixture.capacity.available_permits(), 0);
    supervisor.task.abort();
    let _ = supervisor.task.await;
    fixture.db.close().await;
}

#[tokio::test]
async fn terminal_event_is_not_exposed_before_replay_commit_or_used_as_completion_authority() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Execute(_, _)
    ));
    fixture.db.fail_transition("idle").await;
    let lock = fixture.db.lock(&fixture.session.id).await;
    supervisor.events.send(complete(&id)).unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(fixture.session.snapshot().last_event_id, 0);
    assert!(fixture.session.turn(&id).unwrap().result.is_none());
    assert_eq!(fixture.capacity.available_permits(), 0);
    // The observation may commit, but a separate failed terminal transaction
    // must still hold execution capacity and remain unknown after takeover.
    lock.release().await;
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    status(&fixture.session, SessionStatus::Unknown).await;
    let batch = fixture.session.read_events(0).await.unwrap();
    assert_eq!(batch.events.len(), 1);
    assert!(batch.durable);
    assert_eq!(fixture.capacity.available_permits(), 0);
    supervisor.task.abort();
    let _ = supervisor.task.await;
    fixture.db.expire(&fixture.session.id).await;
    let capacity = Arc::new(Semaphore::new(1));
    let (manager, recovered) = SessionManager::restore(
        8,
        Duration::from_secs(3600),
        fixture.store.clone(),
        &capacity,
    )
    .await
    .unwrap();
    assert!(
        recovered.is_empty(),
        "replay never dispatches an accepted prompt"
    );
    let restored = manager.get(&fixture.session.id).unwrap();
    assert_eq!(restored.read_events(0).await.unwrap().events[0].sequence, 1);
    assert_eq!(restored.snapshot().status, SessionStatus::Unknown);
    assert_eq!(capacity.available_permits(), 0);
    fixture.db.close().await;
}

#[tokio::test]
async fn bounded_replay_survives_takeover_and_sse_cannot_fall_back_to_memory() {
    let fixture = Fixture::new().await;
    // Exercise byte eviction before count eviction.
    let large = Event {
        id: "observation".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "not-an-execution-instruction".into(),
            last_agent_message: Some("a".repeat(60 * 1024)),
        }),
    };
    for _ in 0..20 {
        fixture
            .session
            .observe_committed(&large, true)
            .await
            .unwrap();
    }
    let byte_limited = fixture.session.read_events(0).await.unwrap();
    assert!(byte_limited.gap);
    assert!(byte_limited.events.len() < 20);
    for _ in 0..270 {
        fixture
            .session
            .observe_committed(&complete("observation"), true)
            .await
            .unwrap();
    }
    let oversized = Event {
        id: "oversized".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "observation".into(),
            last_agent_message: Some("secret-body".repeat(MAX_EVENT_BYTES)),
        }),
    };
    fixture
        .session
        .observe_committed(&oversized, true)
        .await
        .unwrap();
    let before = fixture.session.read_events(0).await.unwrap();
    assert_eq!(before.events.len(), MAX_EVENTS);
    assert!(before.gap);
    assert_eq!(
        before.events.last().unwrap().event["msg"]["reason"],
        "event_size_limit"
    );
    assert!(
        !serde_json::to_string(&before)
            .unwrap()
            .contains("secret-body")
    );
    // Destroying the cache must not erase the API's committed history.
    {
        let mut data = fixture.session.data.lock().unwrap();
        data.events.clear();
        data.replay_bytes = 0;
    }
    assert_eq!(
        fixture.session.read_events(0).await.unwrap().events.len(),
        MAX_EVENTS
    );
    fixture.db.expire(&fixture.session.id).await;
    let capacity = Arc::new(Semaphore::new(1));
    let (manager, recovered) = SessionManager::restore(
        8,
        Duration::from_secs(3600),
        fixture.store.clone(),
        &capacity,
    )
    .await
    .unwrap();
    assert!(recovered.is_empty());
    let restored = manager.get(&fixture.session.id).unwrap();
    let after = restored.read_events(before.next_cursor - 1).await.unwrap();
    assert_eq!(after.events[0].sequence, before.next_cursor);
    assert_eq!(after.events[1].sequence, before.next_cursor + 1);
    assert_eq!(
        after.events[1].event["msg"]["reason"],
        "daemon_restart_unobserved_events_possible"
    );
    assert!(restored.read_events(after.next_cursor + 1).await.is_err());
    assert_eq!(
        capacity.available_permits(),
        0,
        "an expired bound process still requires independent containment"
    );
    assert!(
        restored.data.lock().unwrap().active.is_none(),
        "observation turn IDs never manufacture foreground execution"
    );
    assert!(
        fixture
            .session
            .observe_committed(&complete("stale"), true)
            .await
            .is_err()
    );
    assert_eq!(
        restored.read_events(0).await.unwrap().next_cursor,
        after.next_cursor
    );

    let (shutdown, rx) = watch::channel(true);
    let response = crate::sessions::stream::response(
        restored.clone(),
        manager.daemon_id.clone(),
        before.next_cursor - 1,
        rx,
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains(&format!("id: {}", before.next_cursor)));
    assert!(text.contains("daemon_restart_unobserved_events_possible"));
    assert!(!text.contains("replay_unavailable"));
    drop(shutdown);
    match &fixture.db {
        Database::Sqlite(db) => db.pool().close().await,
        Database::Postgres { pool, .. } => pool.close().await,
    }
    let (_shutdown, rx) = watch::channel(true);
    let response = crate::sessions::stream::response(
        restored,
        manager.daemon_id.clone(),
        before.next_cursor - 1,
        rx,
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("event: replay_unavailable"));
    assert!(
        !text.contains("id: "),
        "a failed read cannot advance the durable cursor"
    );
    fixture.db.close().await;
}
