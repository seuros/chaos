use std::sync::Arc;

use chaos_ipc::ProcessId;
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::{JournalStore, SqliteJournalStore};

fn claim_request(id: Uuid, owner: &str) -> ExecutionRequest {
    ExecutionRequest::Claim {
        session_id: id,
        scope: "test".into(),
        owner_id: owner.into(),
        ttl_ms: 30_000,
    }
}

fn claimed(response: ExecutionResponse) -> (ExecutionSession, ExecutionLease) {
    match response {
        ExecutionResponse::Claimed { session, lease } => (session, lease),
        other => panic!("expected claimed session: {other:?}"),
    }
}

fn accept(lease: &ExecutionLease, turn_id: Uuid) -> ExecutionRequest {
    ExecutionRequest::Accept {
        lease: lease.clone(),
        turn_id,
        fingerprint: "a".repeat(64),
    }
}

async fn bound<S: JournalStore>(store: &S) -> ExecutionLease {
    let (_, lease) = claimed(
        store
            .execution(claim_request(Uuid::new_v4(), "first"))
            .await
            .unwrap(),
    );
    store
        .execution(ExecutionRequest::BindProcess {
            lease: lease.clone(),
            process_id: ProcessId::new(),
        })
        .await
        .unwrap();
    lease
}

async fn assert_execution_idempotency<S: JournalStore>(store: &S) {
    let lease = bound(store).await;
    let turn_id = Uuid::new_v4();
    let first = store.execution(accept(&lease, turn_id)).await.unwrap();
    assert!(matches!(
        first,
        ExecutionResponse::Accepted {
            newly_accepted: true,
            ..
        }
    ));
    assert!(matches!(
        store.execution(accept(&lease, turn_id)).await.unwrap(),
        ExecutionResponse::Accepted {
            newly_accepted: false,
            ..
        }
    ));
    let mut changed = accept(&lease, turn_id);
    if let ExecutionRequest::Accept { fingerprint, .. } = &mut changed {
        *fingerprint = "b".repeat(64);
    }
    assert!(matches!(
        store.execution(changed).await,
        Err(JournalError::ExecutionConflict(_))
    ));
    assert!(
        store
            .execution(accept(&lease, Uuid::new_v4()))
            .await
            .is_err()
    );
    assert!(
        store
            .execution(ExecutionRequest::Close {
                lease: lease.clone()
            })
            .await
            .is_err()
    );
    assert!(
        store
            .execution(ExecutionRequest::Release {
                lease: lease.clone()
            })
            .await
            .is_err()
    );

    let dispatch = ExecutionRequest::Dispatch {
        lease: lease.clone(),
        turn_id,
    };
    assert!(matches!(
        store.execution(dispatch.clone()).await.unwrap(),
        ExecutionResponse::Dispatched {
            newly_dispatched: true,
            ..
        }
    ));
    assert!(matches!(
        store.execution(dispatch).await.unwrap(),
        ExecutionResponse::Dispatched {
            newly_dispatched: false,
            ..
        }
    ));
    let finish = ExecutionRequest::Finish {
        lease: lease.clone(),
        turn_id,
        state: ExecutionTurnState::Completed,
        outcome: Some(json!({"result": "done"})),
    };
    let finished = store.execution(finish.clone()).await.unwrap();
    assert_eq!(store.execution(finish).await.unwrap(), finished);
    assert!(matches!(
        store
            .execution(ExecutionRequest::Finish {
                lease: lease.clone(),
                turn_id,
                state: ExecutionTurnState::Failed,
                outcome: None,
            })
            .await,
        Err(JournalError::ExecutionConflict(_))
    ));
    store
        .execution(ExecutionRequest::Close {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        store.execution(accept(&lease, turn_id)).await.unwrap(),
        ExecutionResponse::Accepted {
            turn: ExecutionTurn {
                state: ExecutionTurnState::Completed,
                ..
            },
            newly_accepted: false,
        }
    ));
    store
        .execution(ExecutionRequest::Release {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    let (reopened, _) = claimed(
        store
            .execution(claim_request(lease.session_id, "second"))
            .await
            .unwrap(),
    );
    assert_eq!(reopened.state, ExecutionSessionState::Closed);
    assert!(store.execution(accept(&lease, turn_id)).await.is_err());
}

#[tokio::test]
async fn sqlite_execution_retries_never_redispatch_or_replace_terminal_outcomes() {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    assert_execution_idempotency(&store).await;
}

#[tokio::test]
async fn postgres_execution_retries_never_redispatch_or_replace_terminal_outcomes() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL execution validation; TEST_DATABASE_URL is not set");
        return;
    };
    let pool = chaos_proc::open_runtime_db_postgres_url(&url)
        .await
        .unwrap();
    let store = crate::PostgresJournalStore::new(pool);
    assert_execution_idempotency(&store).await;
}

#[tokio::test]
async fn restart_at_acceptance_and_dispatch_boundaries_is_unknown_not_replayed() {
    for dispatched in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("journal.sqlite");
        let store = SqliteJournalStore::open(&path).await.unwrap();
        let lease = bound(&store).await;
        let turn_id = Uuid::new_v4();
        store.execution(accept(&lease, turn_id)).await.unwrap();
        if dispatched {
            store
                .execution(ExecutionRequest::Dispatch {
                    lease: lease.clone(),
                    turn_id,
                })
                .await
                .unwrap();
        }
        // Deterministic owner loss: expire the stored lease, not a test clock.
        sqlx::query("UPDATE execution_sessions SET expires_at_ms = 0 WHERE session_id = $1")
            .bind(lease.session_id.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        store.pool().close().await;
        drop(store);

        let restarted = SqliteJournalStore::open(&path).await.unwrap();
        let (session, successor) = claimed(
            restarted
                .execution(claim_request(lease.session_id, "successor"))
                .await
                .unwrap(),
        );
        assert_eq!(session.state, ExecutionSessionState::Unknown);
        assert_eq!(session.active_turn_id, Some(turn_id));
        assert_ne!(lease.token, successor.token);
        assert!(matches!(
            restarted
                .execution(accept(&successor, turn_id))
                .await
                .unwrap(),
            ExecutionResponse::Accepted {
                turn: ExecutionTurn {
                    state: ExecutionTurnState::Unknown,
                    ..
                },
                newly_accepted: false,
            }
        ));
        for old_write in [
            ExecutionRequest::Heartbeat {
                lease: lease.clone(),
                ttl_ms: 30_000,
            },
            ExecutionRequest::Dispatch {
                lease: lease.clone(),
                turn_id,
            },
            ExecutionRequest::Finish {
                lease: lease.clone(),
                turn_id,
                state: ExecutionTurnState::Completed,
                outcome: None,
            },
        ] {
            assert!(restarted.execution(old_write).await.is_err());
        }
        assert!(
            restarted
                .execution(ExecutionRequest::Dispatch {
                    lease: successor.clone(),
                    turn_id,
                })
                .await
                .is_err()
        );
        assert!(
            restarted
                .execution(accept(&successor, Uuid::new_v4()))
                .await
                .is_err()
        );
        assert!(
            restarted
                .execution(ExecutionRequest::Close {
                    lease: successor.clone()
                })
                .await
                .is_err()
        );
        // Only an explicit, independently established containment outcome may
        // unblock the session. Taking over/reading/deduplicating never does.
        restarted
            .execution(ExecutionRequest::Finish {
                lease: successor.clone(),
                turn_id,
                state: ExecutionTurnState::Interrupted,
                outcome: Some(json!({"containment": "externally verified"})),
            })
            .await
            .unwrap();
        restarted
            .execution(accept(&successor, Uuid::new_v4()))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn failed_acceptance_or_terminal_write_rolls_back_the_whole_transition() {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    let lease = bound(&store).await;
    let turn_id = Uuid::new_v4();
    sqlx::raw_sql(
        "CREATE TRIGGER fail_accept BEFORE UPDATE OF state ON execution_sessions
         WHEN NEW.state = 'active'
         BEGIN SELECT RAISE(ABORT, 'injected acceptance failure'); END;",
    )
    .execute(store.pool())
    .await
    .unwrap();
    assert!(store.execution(accept(&lease, turn_id)).await.is_err());
    assert!(matches!(
        store
            .execution(ExecutionRequest::GetTurn {
                session_id: lease.session_id,
                turn_id,
            })
            .await,
        Err(JournalError::ExecutionNotFound)
    ));
    sqlx::query("DROP TRIGGER fail_accept")
        .execute(store.pool())
        .await
        .unwrap();
    assert!(matches!(
        store.execution(accept(&lease, turn_id)).await.unwrap(),
        ExecutionResponse::Accepted {
            newly_accepted: true,
            ..
        }
    ));
    store
        .execution(ExecutionRequest::Dispatch {
            lease: lease.clone(),
            turn_id,
        })
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TRIGGER fail_finish BEFORE UPDATE OF state ON execution_sessions
         WHEN NEW.state = 'idle'
         BEGIN SELECT RAISE(ABORT, 'injected terminal failure'); END;",
    )
    .execute(store.pool())
    .await
    .unwrap();
    let finish = ExecutionRequest::Finish {
        lease: lease.clone(),
        turn_id,
        state: ExecutionTurnState::Completed,
        outcome: Some(json!({"result": "done"})),
    };
    assert!(store.execution(finish.clone()).await.is_err());
    assert!(matches!(
        store
            .execution(ExecutionRequest::GetTurn {
                session_id: lease.session_id,
                turn_id,
            })
            .await
            .unwrap(),
        ExecutionResponse::Turn(ExecutionTurn {
            state: ExecutionTurnState::Running,
            outcome: None,
            ..
        })
    ));
    assert!(
        store
            .execution(accept(&lease, Uuid::new_v4()))
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER fail_finish")
        .execute(store.pool())
        .await
        .unwrap();
    store.execution(finish).await.unwrap();
    store
        .execution(accept(&lease, Uuid::new_v4()))
        .await
        .unwrap();
}

#[tokio::test]
async fn terminal_commit_survives_restart_and_idle_process_mapping_is_retained() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("journal.sqlite");
    let store = SqliteJournalStore::open(&path).await.unwrap();
    let lease = bound(&store).await;
    let turn_id = Uuid::new_v4();
    store.execution(accept(&lease, turn_id)).await.unwrap();
    store
        .execution(ExecutionRequest::Dispatch {
            lease: lease.clone(),
            turn_id,
        })
        .await
        .unwrap();
    let finished = store
        .execution(ExecutionRequest::Finish {
            lease: lease.clone(),
            turn_id,
            state: ExecutionTurnState::Completed,
            outcome: Some(json!({"result": "persisted"})),
        })
        .await
        .unwrap();
    store
        .execution(ExecutionRequest::Release {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    store.pool().close().await;
    drop(store);
    let store = SqliteJournalStore::open(&path).await.unwrap();
    let (session, lease) = claimed(
        store
            .execution(claim_request(lease.session_id, "successor"))
            .await
            .unwrap(),
    );
    assert_eq!(session.state, ExecutionSessionState::Idle);
    assert!(session.process_id.is_some());
    assert_eq!(
        store
            .execution(ExecutionRequest::GetTurn {
                session_id: lease.session_id,
                turn_id
            })
            .await
            .unwrap(),
        finished
    );
}

#[tokio::test]
async fn concurrent_claim_and_admission_have_one_winner() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteJournalStore::open(&home.path().join("journal.sqlite"))
            .await
            .unwrap(),
    );
    let id = Uuid::new_v4();
    let (a, b) = tokio::join!(
        store.execution(claim_request(id, "a")),
        store.execution(claim_request(id, "b")),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let (_, lease) = claimed(a.or(b).unwrap());
    let (_, same) = claimed(
        store
            .execution(claim_request(id, &lease.owner_id))
            .await
            .unwrap(),
    );
    assert_eq!(lease, same, "claim retry must not rotate the token");
    store
        .execution(ExecutionRequest::BindProcess {
            lease: lease.clone(),
            process_id: ProcessId::new(),
        })
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        store.execution(accept(&lease, Uuid::new_v4())),
        store.execution(accept(&lease, Uuid::new_v4())),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
}

#[tokio::test]
async fn idle_owner_loss_and_autonomous_work_require_explicit_containment() {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    let original = bound(&store).await;
    sqlx::query("UPDATE execution_sessions SET expires_at_ms = 1 WHERE session_id = $1")
        .bind(original.session_id.to_string())
        .execute(store.pool())
        .await
        .unwrap();
    let (session, lease) = claimed(
        store
            .execution(claim_request(original.session_id, "successor"))
            .await
            .unwrap(),
    );
    assert_eq!(session.state, ExecutionSessionState::Unknown);
    assert_eq!(session.active_turn_id, None);
    assert!(
        store
            .execution(accept(&lease, Uuid::new_v4()))
            .await
            .is_err()
    );
    assert!(
        store
            .execution(ExecutionRequest::Close {
                lease: lease.clone()
            })
            .await
            .is_err()
    );
    assert!(
        store
            .execution(ExecutionRequest::Release {
                lease: lease.clone()
            })
            .await
            .is_err()
    );
    store
        .execution(ExecutionRequest::ReconcileIdle {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    // Autonomous activity can quarantine an otherwise idle control-plane
    // record; no fake HTTP turn is manufactured.
    store
        .execution(ExecutionRequest::Quarantine {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    assert!(
        store
            .execution(accept(&lease, Uuid::new_v4()))
            .await
            .is_err()
    );
    store
        .execution(ExecutionRequest::ReconcileIdle {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    let turn_id = Uuid::new_v4();
    store.execution(accept(&lease, turn_id)).await.unwrap();
    store
        .execution(ExecutionRequest::Quarantine {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    assert!(
        store
            .execution(ExecutionRequest::ReconcileIdle {
                lease: lease.clone()
            })
            .await
            .is_err(),
        "an active unknown turn requires a terminal record"
    );
    assert!(matches!(
        store.execution(accept(&lease, turn_id)).await.unwrap(),
        ExecutionResponse::Accepted {
            turn: ExecutionTurn {
                state: ExecutionTurnState::Unknown,
                ..
            },
            newly_accepted: false,
        }
    ));
    store
        .execution(ExecutionRequest::Finish {
            lease: lease.clone(),
            turn_id,
            state: ExecutionTurnState::Unknown,
            outcome: Some(json!({"partial": "uncertain"})),
        })
        .await
        .unwrap();
    assert!(
        store
            .execution(accept(&lease, Uuid::new_v4()))
            .await
            .is_err()
    );
    store
        .execution(ExecutionRequest::Finish {
            lease: lease.clone(),
            turn_id,
            state: ExecutionTurnState::Interrupted,
            outcome: None,
        })
        .await
        .unwrap();
    store
        .execution(ExecutionRequest::Close {
            lease: lease.clone(),
        })
        .await
        .unwrap();
    // Confirm that closing releases the live-process uniqueness reservation,
    // while retaining the closed session's immutable mapping/tombstones.
    let (_, another) = claimed(
        store
            .execution(claim_request(Uuid::new_v4(), "another"))
            .await
            .unwrap(),
    );
    store
        .execution(ExecutionRequest::BindProcess {
            lease: another,
            process_id: session.process_id.unwrap(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn scope_process_ownership_and_storage_loss_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    let lease = bound(&store).await;
    assert!(
        store
            .execution(ExecutionRequest::Claim {
                session_id: lease.session_id,
                scope: "different".into(),
                owner_id: lease.owner_id.clone(),
                ttl_ms: 30_000,
            })
            .await
            .is_err()
    );
    let (_, second) = claimed(
        store
            .execution(claim_request(Uuid::new_v4(), "second"))
            .await
            .unwrap(),
    );
    let ExecutionResponse::Session(original) = store
        .execution(ExecutionRequest::GetSession {
            session_id: lease.session_id,
        })
        .await
        .unwrap()
    else {
        panic!("expected session")
    };
    assert!(
        store
            .execution(ExecutionRequest::BindProcess {
                lease: second.clone(),
                process_id: original.process_id.unwrap(),
            })
            .await
            .is_err()
    );
    assert!(
        store
            .execution(accept(&second, Uuid::new_v4()))
            .await
            .is_err()
    );
    let turn_id = Uuid::new_v4();
    store.execution(accept(&lease, turn_id)).await.unwrap();
    assert!(
        store
            .execution(ExecutionRequest::Finish {
                lease: lease.clone(),
                turn_id,
                state: ExecutionTurnState::Completed,
                outcome: None,
            })
            .await
            .is_err()
    );
    let mut forged = lease.clone();
    forged.token = Uuid::new_v4();
    forged.expires_at_ms = i64::MAX;
    assert!(
        store
            .execution(ExecutionRequest::Heartbeat {
                lease: forged,
                ttl_ms: 30_000,
            })
            .await
            .is_err()
    );
    store.pool().close().await;
    assert!(
        store
            .execution(accept(&lease, Uuid::new_v4()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn exhausted_turn_capacity_preserves_old_retry_tombstones() {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    let first = bound(&store).await;
    let mut oldest = None;
    for _ in 0..MAX_EXECUTION_TURNS {
        let turn_id = Uuid::new_v4();
        oldest.get_or_insert(turn_id);
        store.execution(accept(&first, turn_id)).await.unwrap();
        store
            .execution(ExecutionRequest::Finish {
                lease: first.clone(),
                turn_id,
                state: ExecutionTurnState::Interrupted,
                outcome: None,
            })
            .await
            .unwrap();
    }
    assert!(
        store
            .execution(accept(&first, Uuid::new_v4()))
            .await
            .is_err()
    );
    assert!(matches!(
        store
            .execution(accept(&first, oldest.unwrap()))
            .await
            .unwrap(),
        ExecutionResponse::Accepted {
            newly_accepted: false,
            ..
        }
    ));
}
