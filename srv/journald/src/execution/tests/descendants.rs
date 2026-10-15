use super::billing::Turn;
use super::*;

fn invocation(request: ExecutionInvocationRequest) -> ExecutionRequest {
    ExecutionRequest::Invocation { request }
}

fn register(turn: &Turn, parent: ProcessId, child: ProcessId) -> ExecutionRequest {
    invocation(ExecutionInvocationRequest::RegisterDescendant {
        lease: turn.lease.clone(),
        root_turn_id: turn.turn_id,
        parent_process_id: parent,
        process_id: child,
    })
}

fn begin(turn: &Turn, child: ProcessId) -> ExecutionRequest {
    invocation(ExecutionInvocationRequest::BeginDescendant {
        lease: turn.lease.clone(),
        root_turn_id: turn.turn_id,
        request: InvocationStart {
            invocation_id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            process_id: child,
            provider_id: "descendant-provider".into(),
            requested_model: "child-model".into(),
            requested_service_tier: None,
            purpose: InvocationPurpose::Sampling,
        },
    })
}

fn complete(turn: &Turn, child: ProcessId, invocation_id: Uuid) -> ExecutionRequest {
    invocation(ExecutionInvocationRequest::CompleteDescendant {
        lease: turn.lease.clone(),
        process_id: child,
        invocation_id,
        receipt: InvocationReceipt {
            completed: true,
            response_id: Some(invocation_id.to_string()),
            server_model: Some("child-observed-model".into()),
            usage: Some(InvocationTokenUsage {
                input_tokens: 7,
                output_tokens: 3,
                total_tokens: 10,
                ..Default::default()
            }),
        },
    })
}

fn began(response: ExecutionResponse) -> ExecutionInvocation {
    let ExecutionResponse::InvocationBegan {
        invocation,
        newly_recorded: true,
    } = response
    else {
        panic!("expected fresh pre-dispatch child invocation")
    };
    invocation
}

async fn assert_lineage_and_fencing<S: JournalStore>(store: &S) {
    let turn = Turn::new(store, Uuid::new_v4().to_string()).await;
    let child = ProcessId::new();
    let grandchild = ProcessId::new();
    let history = ProcessId::new();
    store
        .create_process(crate::CreateProcessInput {
            process_id: history,
            parent: None,
            source: chaos_ipc::protocol::SessionSource::Cli,
            cwd: std::path::PathBuf::from("/"),
            created_at: jiff::Timestamp::now(),
            title: None,
            model_provider: None,
            cli_version: None,
        })
        .await
        .unwrap();
    assert!(
        store
            .execution(register(&turn, turn.process_id, history))
            .await
            .is_err(),
        "unowned journal history is not a fresh descendant process"
    );
    let child_registration = register(&turn, turn.process_id, child);
    let (one, two) = tokio::join!(
        store.execution(child_registration.clone()),
        store.execution(child_registration.clone())
    );
    let results = [one.unwrap(), two.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|response| matches!(
                response,
                ExecutionResponse::DescendantRegistered {
                    newly_registered: true
                }
            ))
            .count(),
        1
    );
    store
        .execution(register(&turn, child, grandchild))
        .await
        .unwrap();
    for (parent, candidate) in [
        (ProcessId::new(), ProcessId::new()),
        (child, child),
        (child, turn.process_id),
        (grandchild, child),
    ] {
        assert!(
            store
                .execution(register(&turn, parent, candidate))
                .await
                .is_err(),
            "unregistered parents, cycles and rewritten parentage must be rejected"
        );
    }
    assert!(
        store
            .execution(begin(&turn, ProcessId::new()))
            .await
            .is_err(),
        "unregistered descendant must not dispatch"
    );
    let mut fenced = turn.lease.clone();
    fenced.token = Uuid::new_v4();
    assert!(
        store
            .execution(invocation(ExecutionInvocationRequest::RegisterDescendant {
                lease: fenced,
                root_turn_id: turn.turn_id,
                parent_process_id: turn.process_id,
                process_id: ProcessId::new(),
            }))
            .await
            .is_err(),
        "descendant registration cannot bypass the root writer fence"
    );
    let request = begin(&turn, child);
    let ExecutionRequest::Invocation {
        request:
            ExecutionInvocationRequest::BeginDescendant {
                request: original, ..
            },
    } = &request
    else {
        unreachable!()
    };
    let recorded = began(store.execution(request.clone()).await.unwrap());
    assert_eq!(recorded.request.turn_id, turn.turn_id);
    assert_eq!(recorded.source_turn_id, Some(original.turn_id));
    assert_eq!(recorded.request.process_id, child);
    assert!(matches!(
        store.execution(request.clone()).await.unwrap(),
        ExecutionResponse::InvocationBegan {
            newly_recorded: false,
            ..
        }
    ));
    let mut changed_source = request;
    if let ExecutionRequest::Invocation {
        request: ExecutionInvocationRequest::BeginDescendant { request, .. },
    } = &mut changed_source
    {
        request.turn_id = Uuid::new_v4();
    }
    assert!(store.execution(changed_source).await.is_err());
    assert!(
        store
            .execution(complete(&turn, grandchild, recorded.request.invocation_id))
            .await
            .is_err(),
        "a registered sibling cannot settle another child's receipt"
    );
    let committed = complete(&turn, child, recorded.request.invocation_id);
    store.execution(committed.clone()).await.unwrap();
    let pending = began(store.execution(begin(&turn, grandchild)).await.unwrap());
    assert!(
        store
            .execution(ExecutionRequest::Close {
                lease: turn.lease.clone()
            })
            .await
            .is_err(),
        "pending child/root execution cannot silently close"
    );
    store
        .execution(ExecutionRequest::Finish {
            lease: turn.lease.clone(),
            turn_id: turn.turn_id,
            state: ExecutionTurnState::Interrupted,
            outcome: None,
        })
        .await
        .unwrap();
    store
        .execution(ExecutionRequest::Close {
            lease: turn.lease.clone(),
        })
        .await
        .unwrap();
    assert!(
        store
            .execution(complete(&turn, grandchild, pending.request.invocation_id))
            .await
            .is_err(),
        "late provider success cannot rewrite a fenced child"
    );
    store.execution(committed).await.unwrap();
    assert!(
        store
            .execution(register(&turn, child, ProcessId::new()))
            .await
            .is_err()
    );
    assert!(store.execution(begin(&turn, child)).await.is_err());
    let accounting = turn.accounting(store).await;
    let summary = accounting.summary.unwrap();
    assert_eq!(
        summary.coverage,
        "root_and_descendant_logical_sampling_and_compaction"
    );
    assert!(!summary.billing_complete);
    assert_eq!(summary.reported_invocations, 1);
    assert_eq!(summary.unknown_invocations, 1);
    let fenced = accounting
        .invocations
        .iter()
        .find(|record| record.request.invocation_id == pending.request.invocation_id)
        .unwrap();
    assert_eq!(fenced.state, InvocationState::Unknown);
    assert!(fenced.receipt.is_none());
    assert_eq!(fenced.source_turn_id, pending.source_turn_id);

    // Even under the same policy, closed child histories never become roots.
    let unbound_id = Uuid::new_v4();
    let (_, lease) = claimed(
        store
            .execution(ExecutionRequest::ClaimBound {
                session_id: unbound_id,
                scope: turn.scope.clone(),
                policy_digest: "a".repeat(64),
                owner_id: "standalone-import".into(),
                ttl_ms: 300_000,
            })
            .await
            .unwrap(),
    );
    for process_id in [child, grandchild] {
        assert!(
            store
                .execution(ExecutionRequest::BindProcess {
                    lease: lease.clone(),
                    process_id,
                })
                .await
                .is_err(),
            "lineage ownership must survive closure permanently"
        );
    }
}

async fn assert_root_binding_and_capacity<S: JournalStore>(store: &S) {
    let mut turn = Turn::new(store, Uuid::new_v4().to_string()).await;
    let child = ProcessId::new();
    store
        .execution(register(&turn, turn.process_id, child))
        .await
        .unwrap();
    let unrelated = Turn::new(store, turn.scope.clone()).await;
    assert!(
        store
            .execution(register(&unrelated, unrelated.process_id, child))
            .await
            .is_err()
    );
    assert!(store.execution(begin(&unrelated, child)).await.is_err());
    assert!(
        store
            .execution(register(&turn, turn.process_id, unrelated.process_id))
            .await
            .is_err(),
        "existing process ownership cannot be adopted as a new descendant"
    );
    let old_turn = turn.turn_id;
    store
        .execution(ExecutionRequest::Finish {
            lease: turn.lease.clone(),
            turn_id: old_turn,
            state: ExecutionTurnState::Completed,
            outcome: None,
        })
        .await
        .unwrap();
    turn.turn_id = Uuid::new_v4();
    store
        .execution(ExecutionRequest::AcceptMetered {
            lease: turn.lease.clone(),
            turn_id: turn.turn_id,
            fingerprint: "e".repeat(64),
        })
        .await
        .unwrap();
    let fresh = ProcessId::new();
    assert!(
        store
            .execution(register(&turn, turn.process_id, fresh))
            .await
            .is_err(),
        "accepted but not dispatched roots cannot boot children"
    );
    store
        .execution(ExecutionRequest::Dispatch {
            lease: turn.lease.clone(),
            turn_id: turn.turn_id,
        })
        .await
        .unwrap();
    assert!(
        store.execution(begin(&turn, child)).await.is_err(),
        "inherited child capability cannot cross root turns"
    );
    assert!(
        store
            .execution(register(&turn, turn.process_id, child))
            .await
            .is_err()
    );
    assert!(
        store
            .execution(register(&turn, child, fresh))
            .await
            .is_err(),
        "old descendants cannot parent new descendants in a later turn"
    );
    store
        .execution(register(&turn, turn.process_id, fresh))
        .await
        .unwrap();
    for _ in 1..crate::execution::invocations::MAX_EXECUTION_INVOCATIONS {
        store
            .execution(register(&turn, fresh, ProcessId::new()))
            .await
            .unwrap();
    }
    assert!(
        store
            .execution(register(&turn, fresh, ProcessId::new()))
            .await
            .is_err(),
        "process 129 must be rejected before boot"
    );
    assert!(matches!(
        store
            .execution(register(&turn, turn.process_id, fresh))
            .await
            .unwrap(),
        ExecutionResponse::DescendantRegistered {
            newly_registered: false
        }
    ));
}

macro_rules! backend_cases {
    ($sqlite:ident, $postgres:ident, $assertion:ident) => {
        #[tokio::test]
        async fn $sqlite() {
            let home = tempfile::tempdir().unwrap();
            let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
                .await
                .unwrap();
            $assertion(&store).await;
            assert_legacy_default(&store).await;
        }

        #[tokio::test]
        async fn $postgres() {
            let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
                eprintln!("skipping PostgreSQL lineage validation; TEST_DATABASE_URL is not set");
                return;
            };
            let pool = chaos_proc::open_runtime_db_postgres_url(&url)
                .await
                .unwrap();
            let store = crate::PostgresJournalStore::new(pool);
            $assertion(&store).await;
            assert_legacy_default_postgres(&store).await;
        }
    };
}

// Omit the new column exactly as old writers did. Migration defaults must never
// retroactively upgrade their accounting coverage to include descendants.
macro_rules! legacy_default {
    ($name:ident, $store:ty) => {
        async fn $name(store: &$store) {
            let turn = Turn::new(store, Uuid::new_v4().to_string()).await;
            let legacy = Uuid::new_v4();
            sqlx::query("INSERT INTO execution_turns (session_id,turn_id,fingerprint,state,accounting_enabled) VALUES ($1,$2,$3,'completed',1)")
                .bind(turn.lease.session_id.to_string())
                .bind(legacy.to_string())
                .bind("f".repeat(64))
                .execute(store.pool()).await.unwrap();
            let ExecutionResponse::Accounting(accounting) = store.execution(invocation(
                ExecutionInvocationRequest::Read { session_id: turn.lease.session_id, turn_id: legacy }
            )).await.unwrap() else { panic!("expected legacy accounting") };
            assert_eq!(accounting.summary.unwrap().coverage, "direct_logical_sampling_and_compaction");
        }
    };
}

legacy_default!(assert_legacy_default, SqliteJournalStore);
legacy_default!(assert_legacy_default_postgres, crate::PostgresJournalStore);
backend_cases!(
    sqlite_nested_descendants_preserve_provenance_and_fence_after_closure,
    postgres_nested_descendants_preserve_provenance_and_fence_after_closure,
    assert_lineage_and_fencing
);
backend_cases!(
    sqlite_descendants_cannot_cross_turns_or_exceed_boot_capacity,
    postgres_descendants_cannot_cross_turns_or_exceed_boot_capacity,
    assert_root_binding_and_capacity
);
