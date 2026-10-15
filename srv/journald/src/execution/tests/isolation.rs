use super::*;

fn scoped_claim(id: Uuid, scope: &str, digest: &str) -> ExecutionRequest {
    ExecutionRequest::ClaimBound {
        session_id: id,
        scope: scope.into(),
        policy_digest: digest.into(),
        owner_id: "isolated-owner".into(),
        ttl_ms: 30_000,
    }
}

async fn assert_scope_isolation<S: JournalStore>(store: &S) {
    let scope = Uuid::new_v4().to_string();
    let (a, b) = tokio::join!(
        store.execution(ExecutionRequest::BindScope {
            scope: scope.clone(),
            policy_digest: "a".repeat(64)
        }),
        store.execution(ExecutionRequest::BindScope {
            scope: scope.clone(),
            policy_digest: "b".repeat(64)
        }),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let ExecutionResponse::Scope(Some(digest)) = a.or(b).unwrap() else {
        panic!("expected policy")
    };
    let id = Uuid::new_v4();
    assert!(
        store
            .execution(ExecutionRequest::Claim {
                session_id: id,
                scope: scope.clone(),
                owner_id: "bypass".into(),
                ttl_ms: 30_000,
            })
            .await
            .is_err(),
        "legacy claims cannot bypass a bound scope"
    );
    let (_, lease) = claimed(
        store
            .execution(scoped_claim(id, &scope, &digest))
            .await
            .unwrap(),
    );
    let process_id = ProcessId::new();
    assert!(
        store
            .execution(ExecutionRequest::BindProcess {
                lease: lease.clone(),
                process_id,
            })
            .await
            .is_err(),
        "unverified journal history must not be implicitly adopted"
    );
    store
        .execution(ExecutionRequest::BindNewProcess {
            lease: lease.clone(),
            process_id,
        })
        .await
        .unwrap();
    store
        .execution(ExecutionRequest::Close {
            lease: lease.clone(),
        })
        .await
        .unwrap();

    // Closure releases the live-consumer reservation, NOT tenant ownership.
    let other_scope = Uuid::new_v4().to_string();
    store
        .execution(ExecutionRequest::BindScope {
            scope: other_scope.clone(),
            policy_digest: digest.clone(),
        })
        .await
        .unwrap();
    let (_, other) = claimed(
        store
            .execution(scoped_claim(Uuid::new_v4(), &other_scope, &digest))
            .await
            .unwrap(),
    );
    for bind in [
        ExecutionRequest::BindProcess {
            lease: other.clone(),
            process_id,
        },
        ExecutionRequest::BindNewProcess {
            lease: other.clone(),
            process_id,
        },
    ] {
        assert!(
            store.execution(bind).await.is_err(),
            "another tenant cannot relabel closed history"
        );
    }
    let (_, same) = claimed(
        store
            .execution(scoped_claim(Uuid::new_v4(), &scope, &digest))
            .await
            .unwrap(),
    );
    store
        .execution(ExecutionRequest::BindProcess {
            lease: same,
            process_id,
        })
        .await
        .unwrap();
    assert!(
        store
            .execution(ExecutionRequest::BindScope {
                scope: scope.clone(),
                policy_digest: "c".repeat(64),
            })
            .await
            .is_err(),
        "even a closed namespace cannot change its execution policy"
    );

    // Empty-scope binding races a legacy creator: exactly one may commit.
    let legacy_scope = Uuid::new_v4().to_string();
    let (bind, claim) = tokio::join!(
        store.execution(ExecutionRequest::BindScope {
            scope: legacy_scope.clone(),
            policy_digest: digest.clone(),
        }),
        store.execution(ExecutionRequest::Claim {
            session_id: Uuid::new_v4(),
            scope: legacy_scope,
            owner_id: "legacy".into(),
            ttl_ms: 30_000,
        }),
    );
    assert_eq!(
        usize::from(bind.is_ok()) + usize::from(claim.is_ok()),
        1,
        "policy bootstrap and unverified history creation must serialize"
    );

    let legacy_scope = Uuid::new_v4().to_string();
    store
        .execution(ExecutionRequest::Claim {
            session_id: Uuid::new_v4(),
            scope: legacy_scope.clone(),
            owner_id: "legacy".into(),
            ttl_ms: 30_000,
        })
        .await
        .unwrap();
    assert!(
        store
            .execution(ExecutionRequest::BindScope {
                scope: legacy_scope,
                policy_digest: digest,
            })
            .await
            .is_err(),
        "legacy records have no verified policy to adopt"
    );
}

#[tokio::test]
async fn sqlite_policy_bootstrap_and_history_ownership_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    assert_scope_isolation(&store).await;
}

#[tokio::test]
async fn postgres_policy_bootstrap_and_history_ownership_fail_closed() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL isolation validation; TEST_DATABASE_URL is not set");
        return;
    };
    let pool = chaos_proc::open_runtime_db_postgres_url(&url)
        .await
        .unwrap();
    assert_scope_isolation(&crate::PostgresJournalStore::new(pool)).await;
}
