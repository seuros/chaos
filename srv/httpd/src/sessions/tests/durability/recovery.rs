use super::*;
use crate::sessions::durable::recovery::ResolveRequest;
use rama::Service;
use rama::http::body::util::BodyExt;
use rama::http::{Body, Request};
use serde_json::{Value, json};

impl Fixture {
    async fn quarantined(turn: bool) -> (Self, Value) {
        let mut fixture = Self::new().await;
        let turn_id = if turn {
            let id = Uuid::new_v4().to_string();
            fixture.admit(&id).await.unwrap();
            Some(id)
        } else {
            fixture.session.data.lock().unwrap().orphan_permit =
                Some(fixture.capacity.clone().try_acquire_owned().unwrap());
            None
        };
        fixture
            .session
            .ledger
            .get()
            .unwrap()
            .quarantine()
            .await
            .unwrap();
        fixture.session.data.lock().unwrap().mark_unknown();
        drop(fixture.commands.take());
        let body = json!({
            "resolution_id": Uuid::new_v4(),
            "expected_process_id": fixture.session.snapshot().process_id,
            "expected_turn_id": turn_id,
            "expected_recovery_epoch": fixture.epoch().await,
            "containment_confirmed": true,
            "actor": "operator",
            "reason": "Executor and external tools independently contained",
        });
        (fixture, body)
    }

    async fn epoch(&self) -> u64 {
        let ExecutionResponse::Session(record) = self
            .store
            .call(ExecutionRequest::GetSession {
                session_id: Uuid::parse_str(&self.session.id).unwrap(),
            })
            .await
            .unwrap()
        else {
            panic!("expected session")
        };
        record.recovery_epoch
    }

    async fn restored(&self) -> (Arc<SessionManager>, Arc<Session>, Arc<Semaphore>) {
        let capacity = Arc::new(Semaphore::new(1));
        let (manager, recovered) =
            SessionManager::restore(8, Duration::from_secs(3600), self.store.clone(), &capacity)
                .await
                .unwrap();
        assert!(recovered.is_empty(), "recovery never boots an executor");
        let session = manager.get(&self.session.id).unwrap();
        (Arc::new(manager), session, capacity)
    }
}

async fn serve(
    state: &Arc<crate::ServerState>,
    method: &str,
    session: &Session,
    body: Body,
    operator: bool,
) -> rama::http::Response {
    let manager = state.sessions.as_ref().unwrap();
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/sessions/{}/recovery", session.id))
        .header("authorization", "Bearer test-token")
        .header("x-chaos-daemon-id", &manager.daemon_id)
        .header("content-type", "application/json");
    if operator {
        request = request.header(
            "x-chaos-recovery-authorization",
            "Bearer recovery-test-token",
        );
    }
    crate::api::http_service(state.clone())
        .serve(request.body(body).unwrap())
        .await
        .unwrap()
}

async fn state(fixture: &Fixture) -> (tempfile::TempDir, Arc<crate::ServerState>) {
    let (home, mut state) = crate::test_support::server_state(8192, false).await;
    let unique = Arc::get_mut(&mut state).unwrap();
    unique.sessions = Some(fixture.manager.clone());
    unique.semaphore = fixture.capacity.clone();
    unique.recovery_token = Some(Arc::from("recovery-test-token"));
    (home, state)
}

#[tokio::test]
async fn recovery_rejects_ordinary_credentials_and_stale_or_unconfirmed_targets() {
    let (fixture, body) = Fixture::quarantined(true).await;
    let (_home, state) = state(&fixture).await;
    let inspection = serve(&state, "GET", &fixture.session, Body::empty(), false).await;
    assert_eq!(inspection.status(), StatusCode::OK);
    let bytes = inspection.into_body().collect().await.unwrap().to_bytes();
    let view: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(view["recovery"]["can_resolve"], true);
    assert_eq!(
        view["recovery"]["session"]["recovery_epoch"],
        body["expected_recovery_epoch"]
    );
    assert!(
        !String::from_utf8_lossy(&bytes).contains("lease_token"),
        "inspection must not expose control-plane authority"
    );
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let response = serve(
        &state,
        "POST",
        &fixture.session,
        crate::test_support::counting_body(&polls),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        polls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "operator authorization must precede body polling"
    );
    for (field, value, expected) in [
        (
            "containment_confirmed",
            json!(false),
            StatusCode::BAD_REQUEST,
        ),
        (
            "expected_process_id",
            json!(ProcessId::new()),
            StatusCode::CONFLICT,
        ),
        (
            "expected_turn_id",
            json!(Uuid::new_v4()),
            StatusCode::CONFLICT,
        ),
    ] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(
            serve(
                &state,
                "POST",
                &fixture.session,
                Body::from(changed.to_string()),
                true
            )
            .await
            .status(),
            expected
        );
    }
    assert_eq!(fixture.capacity.available_permits(), 0);
    assert_eq!(fixture.session.snapshot().status, SessionStatus::Unknown);
    assert!(matches!(
        fixture
            .store
            .call(ExecutionRequest::GetResolution {
                session_id: Uuid::parse_str(&fixture.session.id).unwrap()
            })
            .await
            .unwrap(),
        ExecutionResponse::Resolution(None)
    ));
    fixture.db.close().await;
}

#[tokio::test]
async fn recovery_cannot_resolve_a_session_with_a_live_supervisor_receiver() {
    let mut fixture = Fixture::new().await;
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    fixture
        .session
        .ledger
        .get()
        .unwrap()
        .quarantine()
        .await
        .unwrap();
    fixture.session.data.lock().unwrap().mark_unknown();
    let request = ResolveRequest {
        resolution_id: Uuid::new_v4(),
        expected_process_id: fixture
            .session
            .snapshot()
            .process_id
            .map(|id| ProcessId::from_string(&id).unwrap()),
        expected_turn_id: Some(Uuid::parse_str(&id).unwrap()),
        expected_recovery_epoch: fixture.epoch().await,
        containment_confirmed: true,
        actor: "operator".into(),
        reason: "independently contained".into(),
    };
    assert_eq!(
        fixture.session.resolve(request).await.unwrap_err().code,
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.capacity.available_permits(), 0);
    assert!(
        matches!(
            fixture.commands.as_mut().unwrap().try_recv().unwrap(),
            Command::Turn(_)
        ),
        "pending input cannot lose its reservation"
    );
    fixture.db.close().await;
}

#[tokio::test]
async fn failed_resolution_rolls_back_audit_terminal_outcome_and_capacity_release() {
    let (fixture, body) = Fixture::quarantined(true).await;
    fixture.db.fail_transition("closed").await;
    let mut request = serde_json::from_value::<ResolveRequest>(body).unwrap();
    assert!(fixture.session.resolve(request.clone()).await.is_err());
    assert_eq!(fixture.capacity.available_permits(), 0);
    let id = request.expected_turn_id.unwrap().to_string();
    assert_eq!(
        fixture.persisted_turn(&id).await.state,
        ExecutionTurnState::Unknown
    );
    assert!(matches!(
        fixture
            .store
            .call(ExecutionRequest::GetResolution {
                session_id: Uuid::parse_str(&fixture.session.id).unwrap()
            })
            .await
            .unwrap(),
        ExecutionResponse::Resolution(None)
    ));
    fixture.db.clear_transition_fault().await;
    assert!(
        fixture.session.resolve(request.clone()).await.is_err(),
        "a failed writer must not resume writes with its abandoned lease"
    );
    fixture.db.expire(&fixture.session.id).await;
    let (_manager, recovered, capacity) = fixture.restored().await;
    assert_eq!(capacity.available_permits(), 0);
    assert_eq!(
        recovered.resolve(request.clone()).await.unwrap_err().code,
        StatusCode::CONFLICT,
        "a takeover requires fresh inspection and containment confirmation"
    );
    request.expected_recovery_epoch = fixture.epoch().await;
    request.resolution_id = Uuid::new_v4();
    let record = recovered.resolve(request.clone()).await.unwrap();
    assert_eq!(capacity.available_permits(), 1);
    assert_eq!(recovered.snapshot().status, SessionStatus::Closed);
    // The previous owner may read the same committed resolution, not invent one.
    assert_eq!(fixture.session.resolve(request).await.unwrap(), record);
    assert_eq!(fixture.capacity.available_permits(), 1);
    fixture.db.close().await;
}

#[tokio::test]
async fn disconnected_resolution_is_audited_retry_safe_and_never_replays_after_restart() {
    let (fixture, body) = Fixture::quarantined(true).await;
    let (_home, state) = state(&fixture).await;
    let write_lock = fixture.db.lock(&fixture.session.id).await;
    let call_state = state.clone();
    let session = fixture.session.clone();
    let call_body = body.clone();
    let caller = tokio::spawn(async move {
        serve(
            &call_state,
            "POST",
            &session,
            Body::from(call_body.to_string()),
            true,
        )
        .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_until(&fixture.session, |_| {
            fixture.session.admission.try_lock().is_err()
        }),
    )
    .await
    .unwrap();
    caller.abort();
    let _ = caller.await;
    assert_eq!(fixture.manager.ingress.available_permits(), 15);
    write_lock.release().await;
    status(&fixture.session, SessionStatus::Closed).await;
    assert_eq!(fixture.capacity.available_permits(), 1);
    let resolution: ResolveRequest = serde_json::from_value(body.clone()).unwrap();
    let record = fixture.session.resolve(resolution.clone()).await.unwrap();
    let (_manager, restored, capacity) = fixture.restored().await;
    assert_eq!(capacity.available_permits(), 1);
    assert_eq!(restored.resolve(resolution).await.unwrap(), record);
    let mut changed = body;
    changed["reason"] = json!("tampered decision");
    assert_eq!(
        restored
            .resolve(serde_json::from_value(changed).unwrap())
            .await
            .unwrap_err()
            .code,
        StatusCode::CONFLICT
    );
    let old = record.request.expected_turn_id.unwrap().to_string();
    assert_eq!(
        restored
            .admit(old, request("hello"), &capacity, Duration::from_secs(60))
            .await
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    assert!(
        restored
            .admit(
                Uuid::new_v4().to_string(),
                request("next"),
                &capacity,
                Duration::from_secs(60)
            )
            .await
            .is_err()
    );
    assert!(state.process_table.list_process_ids().await.is_empty());
    fixture.db.close().await;
}

#[tokio::test]
async fn no_turn_recovery_audits_and_releases_only_the_orphan_reservation() {
    let (fixture, mut body) = Fixture::quarantined(false).await;
    assert_eq!(fixture.capacity.available_permits(), 0);
    // A new quarantine with identical process/turn IDs still invalidates the
    // old assertion. Matching IDs alone are not a containment precondition.
    fixture
        .session
        .ledger
        .get()
        .unwrap()
        .quarantine()
        .await
        .unwrap();
    assert_eq!(
        fixture
            .session
            .resolve(serde_json::from_value(body.clone()).unwrap())
            .await
            .unwrap_err()
            .code,
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.capacity.available_permits(), 0);
    body["expected_recovery_epoch"] = json!(fixture.epoch().await);
    body["resolution_id"] = json!(Uuid::new_v4());
    let record = fixture
        .session
        .resolve(serde_json::from_value(body).unwrap())
        .await
        .unwrap();
    assert_eq!(record.request.expected_turn_id, None);
    assert_eq!(record.request.outcome, None);
    assert_eq!(fixture.capacity.available_permits(), 1);
    let (_manager, recovered, capacity) = fixture.restored().await;
    assert_eq!(recovered.snapshot().status, SessionStatus::Closed);
    assert_eq!(capacity.available_permits(), 1);
    fixture.db.close().await;
}
