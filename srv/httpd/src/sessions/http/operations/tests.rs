use super::*;
use crate::sessions::durable::{Client, Store};
use crate::sessions::tests::durability::storage::Database;
use rama::Service;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::sync::Semaphore;

async fn call(
    state: &Arc<ServerState>,
    method: &str,
    path: &str,
    operator: bool,
    generation: Option<&str>,
    body: Body,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json");
    if operator {
        request = request.header("x-chaos-recovery-authorization", "Bearer operator-token");
    }
    if let Some(generation) = generation {
        request = request.header("x-chaos-daemon-id", generation);
    }
    crate::api::http_service(state.clone())
        .serve(request.body(body).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn postgres_evidence_requires_operator_before_body_and_survives_nonexecuting_drain() {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        tracing::info!("skipping PostgreSQL operator evidence; TEST_DATABASE_URL is not set");
        return;
    }
    let (home, mut state) = crate::test_support::server_state(8192, false).await;
    let (db, client) = Database::open(home.path()).await;
    let store = Arc::new(Store {
        client: client.clone(),
        scope: "financial".into(),
        policy_digest: Some("a".repeat(64)),
        policy: None,
        recovery_only: false,
    });
    let capacity = Arc::new(Semaphore::new(1));
    let (manager, recovered) =
        SessionManager::restore(8, Duration::from_secs(3600), store, &capacity)
            .await
            .unwrap();
    assert!(recovered.is_empty());
    let generation = manager.daemon_id.clone();
    let unique = Arc::get_mut(&mut state).unwrap();
    unique.sessions = Some(Arc::new(manager));
    unique.recovery_token = Some(Arc::from("operator-token"));
    let polls = Arc::new(AtomicUsize::new(0));
    let archive = format!("/api/sessions/{}/archive", Uuid::new_v4());
    let billing = format!(
        "/api/sessions/{}/turns/{}/billing",
        Uuid::new_v4(),
        Uuid::new_v4()
    );
    for (method, path) in [
        ("PUT", "/api/sessions/prices/v1"),
        ("GET", "/api/sessions/prices/v1"),
        ("POST", archive.as_str()),
        ("GET", archive.as_str()),
        ("POST", billing.as_str()),
        ("GET", billing.as_str()),
    ] {
        let response = call(
            &state,
            method,
            path,
            false,
            Some(&generation),
            crate::test_support::counting_body(&polls),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/sessions/prices/v1",
            true,
            Some("old-generation"),
            crate::test_support::counting_body(&polls)
        )
        .await
        .status(),
        StatusCode::CONFLICT,
    );
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let schedule = json!({
        "version":"v1", "provider_id":"mock", "model":"mock", "service_tier":"standard",
        "currency":"USD", "tokens_per_unit":1000000,
        "prices":{"uncached_input":1000000000,"cache_creation_input":2000000000,
            "cached_input":500000000,"ordinary_output":3000000000_u64,"reasoning_output":3000000000_u64},
        "source_sha256":"b".repeat(64), "actor":"operator", "reason":"Verified test-only rate source"
    });
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/sessions/prices/v1",
            true,
            Some(&generation),
            Body::from(schedule.to_string())
        )
        .await
        .status(),
        StatusCode::OK
    );
    // Drain with a fresh generation: no execution policy/provider credentials
    // need to be restored merely to read or append immutable operator evidence.
    let drain = Arc::new(Store {
        client,
        scope: "financial".into(),
        policy_digest: Some("a".repeat(64)),
        policy: None,
        recovery_only: true,
    });
    let (manager, recovered) =
        SessionManager::restore(8, Duration::from_secs(3600), drain, &capacity)
            .await
            .unwrap();
    assert!(recovered.is_empty());
    let generation = manager.daemon_id.clone();
    Arc::get_mut(&mut state).unwrap().sessions = Some(Arc::new(manager));
    assert_eq!(
        call(
            &state,
            "GET",
            "/api/sessions/prices/v1",
            true,
            None,
            Body::empty()
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/sessions/prices/v1",
            true,
            Some(&generation),
            Body::from(schedule.to_string())
        )
        .await
        .status(),
        StatusCode::OK
    );
    let mut changed = schedule;
    changed["currency"] = json!("EUR");
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/sessions/prices/v1",
            true,
            Some(&generation),
            Body::from(changed.to_string())
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(capacity.available_permits(), 1);
    assert!(state.sessions.as_ref().unwrap().snapshots().is_empty());
    db.close().await;
}

#[tokio::test]
async fn sqlite_financial_routes_are_absent_before_body_generation_or_capacity_checks() {
    let (home, mut state) = crate::test_support::server_state(8192, false).await;
    let db = Arc::new(
        chaos_journald::SqliteJournalStore::open(&home.path().join("local.sqlite"))
            .await
            .unwrap(),
    );
    let polls = Arc::new(AtomicUsize::new(0));
    let billing = format!(
        "/api/sessions/{}/turns/{}/billing",
        Uuid::new_v4(),
        Uuid::new_v4()
    );
    for recovery_only in [false, true] {
        let store = Arc::new(Store {
            // Exercise the production SQLite transport, not just the in-process
            // test store. No request may reach this nonexistent journal socket.
            client: Client::Journal(chaos_journald::JournalClient::rpc(
                chaos_journald::JournalRpcClient::new(home.path().join("absent.sock")),
            )),
            scope: "local".into(),
            policy_digest: None,
            policy: None,
            recovery_only,
        });
        // The manager is deliberately not restored: route absence must not
        // depend on contacting storage or booting a process.
        let mut manager = SessionManager::new(8, Duration::from_secs(3600));
        manager.store = Some(store);
        let unique = Arc::get_mut(&mut state).unwrap();
        unique.sessions = Some(Arc::new(manager));
        unique.recovery_token = Some(Arc::from("operator-token"));
        let manager = state.sessions.as_ref().unwrap();
        let _capacity = manager
            .ingress
            .clone()
            .try_acquire_many_owned(manager.ingress.available_permits() as u32)
            .unwrap();
        for path in ["/api/sessions/prices/v1", billing.as_str()] {
            for method in ["GET", "POST", "PUT", "DELETE"] {
                for generation in [None, Some("stale-generation")] {
                    let response = call(
                        &state,
                        method,
                        path,
                        true,
                        generation,
                        crate::test_support::counting_body(&polls),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::NOT_FOUND);
                }
            }
        }
    }
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table'
         AND name IN ('execution_price_schedules','execution_reconciliations')",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(tables.is_empty());
}
