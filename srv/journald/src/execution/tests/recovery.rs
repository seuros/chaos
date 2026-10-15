use super::*;

async fn assert_operator_resolution<S: JournalStore>(store: &S) {
    let lease = bound(store).await;
    let turn_id = Uuid::new_v4();
    store.execution(accept(&lease, turn_id)).await.unwrap();
    let ExecutionResponse::Session(session) = store
        .execution(ExecutionRequest::GetSession {
            session_id: lease.session_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected session")
    };
    let mut requested = ExecutionResolution {
        resolution_id: Uuid::new_v4(),
        expected_process_id: session.process_id,
        expected_turn_id: Some(turn_id),
        expected_recovery_epoch: session.recovery_epoch,
        containment_confirmed: true,
        actor: "operator".into(),
        reason: "Executor and external tools independently contained".into(),
        outcome: Some(json!({"partial": "retained"})),
    };
    let resolve = |resolution| ExecutionRequest::Resolve {
        lease: lease.clone(),
        resolution,
    };
    assert!(
        store.execution(resolve(requested.clone())).await.is_err(),
        "a live accepted session cannot be resolved"
    );
    store
        .execution(ExecutionRequest::Quarantine {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    let ExecutionResponse::Session(session) = store
        .execution(ExecutionRequest::GetSession {
            session_id: lease.session_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected session")
    };
    assert!(
        store.execution(resolve(requested.clone())).await.is_err(),
        "the same process/turn mapping cannot reuse a stale quarantine epoch"
    );
    requested.expected_recovery_epoch = session.recovery_epoch;
    let mut changed = requested.clone();
    changed.expected_turn_id = Some(Uuid::new_v4());
    assert!(store.execution(resolve(changed)).await.is_err());
    let mut unconfirmed = requested.clone();
    unconfirmed.containment_confirmed = false;
    assert!(store.execution(resolve(unconfirmed)).await.is_err());
    let mut forged = lease.clone();
    forged.token = Uuid::new_v4();
    assert!(
        store
            .execution(ExecutionRequest::Resolve {
                lease: forged,
                resolution: requested.clone()
            })
            .await
            .is_err(),
        "fenced writers cannot resolve unknown work"
    );

    // Different decisions race: exactly one audit and closure can commit.
    let mut alternative = requested.clone();
    alternative.resolution_id = Uuid::new_v4();
    alternative.reason = "A competing resolution".into();
    let (a, b) = tokio::join!(
        store.execution(resolve(requested)),
        store.execution(resolve(alternative)),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let ExecutionResponse::Resolved(record) = a.or(b).unwrap() else {
        panic!("expected resolution")
    };
    let committed = ExecutionResponse::Resolved(record.clone());
    // Audit-bound retry succeeds without renewing the atomically released lease.
    assert_eq!(
        store
            .execution(resolve(record.request.clone()))
            .await
            .unwrap(),
        committed
    );
    assert_eq!(
        store
            .execution(ExecutionRequest::GetResolution {
                session_id: lease.session_id
            })
            .await
            .unwrap(),
        ExecutionResponse::Resolution(Some(record.clone()))
    );
    let ExecutionResponse::Session(session) = store
        .execution(ExecutionRequest::GetSession {
            session_id: lease.session_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected session")
    };
    assert_eq!(session.state, ExecutionSessionState::Closed);
    assert_eq!(session.active_turn_id, None);
    assert_eq!(session.turn_count, 1);
    assert!(matches!(
        store
            .execution(ExecutionRequest::GetTurn {
                session_id: lease.session_id,
                turn_id
            })
            .await
            .unwrap(),
        ExecutionResponse::Turn(ExecutionTurn {
            state: ExecutionTurnState::Interrupted,
            ..
        })
    ));
    assert!(
        store
            .execution(accept(&lease, Uuid::new_v4()))
            .await
            .is_err()
    );
    // Fail the LAST write (audit insertion), after closure was staged. Its
    // duplicate id must roll back closure and keep the second process reserved.
    let second = bound(store).await;
    let ExecutionResponse::Session(quarantined) = store
        .execution(ExecutionRequest::Quarantine {
            lease: second.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected quarantine")
    };
    let mut duplicate_id = record.request.clone();
    duplicate_id.expected_process_id = quarantined.process_id;
    duplicate_id.expected_turn_id = None;
    duplicate_id.expected_recovery_epoch = quarantined.recovery_epoch;
    duplicate_id.outcome = None;
    assert!(
        store
            .execution(ExecutionRequest::Resolve {
                lease: second.clone(),
                resolution: duplicate_id
            })
            .await
            .is_err()
    );
    let ExecutionResponse::Session(unchanged) = store
        .execution(ExecutionRequest::GetSession {
            session_id: second.session_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected session")
    };
    assert_eq!(unchanged.state, ExecutionSessionState::Unknown);
    assert_eq!(
        store
            .execution(ExecutionRequest::GetResolution {
                session_id: second.session_id
            })
            .await
            .unwrap(),
        ExecutionResponse::Resolution(None)
    );
    let mut tampered = record.request;
    tampered.actor = "different operator".into();
    assert!(store.execution(resolve(tampered)).await.is_err());
}

#[tokio::test]
async fn sqlite_operator_resolution_is_fenced_atomic_and_audit_bound() {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    assert_operator_resolution(&store).await;
}

#[tokio::test]
async fn postgres_operator_resolution_is_fenced_atomic_and_audit_bound() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL resolution validation; TEST_DATABASE_URL is not set");
        return;
    };
    let pool = chaos_proc::open_runtime_db_postgres_url(&url)
        .await
        .unwrap();
    let store = crate::PostgresJournalStore::new(pool);
    assert_operator_resolution(&store).await;
}
