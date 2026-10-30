use super::*;
use crate::sessions::durable::policy::PinnedPolicy;
use rama::Service;
use rama::http::Request;
use serde_json::json;

fn store_like(fixture: &Fixture, digest: Option<String>, recovery_only: bool) -> Arc<Store> {
    Arc::new(Store {
        client: fixture.store.client.clone(),
        scope: fixture.store.scope.clone(),
        policy_digest: digest,
        policy: None,
        recovery_only,
    })
}

#[tokio::test]
async fn changed_credentials_cwd_permissions_tools_or_clamp_cannot_take_over_retained_history() {
    let (_home, state) = crate::test_support::server_state(8192, false).await;
    let original =
        Arc::new(PinnedPolicy::new(state.config.clone(), state.auth_manager.clone()).unwrap());
    let fixture = Fixture::with_policy(Some(original.clone())).await;
    let capacity = Arc::new(Semaphore::new(1));
    let mut candidates = Vec::new();
    let mut provider = state.config.as_ref().clone();
    provider.model_provider.base_url = Some("https://changed-provider.invalid".into());
    candidates.push(Arc::new(
        PinnedPolicy::new(Arc::new(provider), state.auth_manager.clone()).unwrap(),
    ));
    let mut cwd = state.config.as_ref().clone();
    cwd.cwd = cwd.cwd.join("different-tenant");
    candidates.push(Arc::new(
        PinnedPolicy::new(Arc::new(cwd), state.auth_manager.clone()).unwrap(),
    ));
    let mut permissions = state.config.as_ref().clone();
    permissions.permissions.allow_login_shell = !permissions.permissions.allow_login_shell;
    candidates.push(Arc::new(
        PinnedPolicy::new(Arc::new(permissions), state.auth_manager.clone()).unwrap(),
    ));
    let mut tools = state.config.as_ref().clone();
    let mut servers = tools.mcp_servers.get().clone();
    servers.insert(
        "new-tool".into(),
        serde_json::from_value(json!({
            "command": "unexecuted-server", "args": [],
        }))
        .unwrap(),
    );
    tools.mcp_servers.set(servers).unwrap();
    candidates.push(Arc::new(
        PinnedPolicy::new(Arc::new(tools), state.auth_manager.clone()).unwrap(),
    ));
    let mut clamp = state.config.as_ref().clone();
    clamp.antigravity.model = Some("different-subscription-route".into());
    candidates.push(Arc::new(
        PinnedPolicy::new(Arc::new(clamp), state.auth_manager.clone()).unwrap(),
    ));
    let changed_auth = chaos_kern::test_support::auth_manager_from_auth_with_home(
        chaos_kern::ChaosAuth::from_api_key("different-test-credential"),
        state.config.chaos_home.clone(),
    );
    candidates.push(Arc::new(
        PinnedPolicy::new(state.config.clone(), changed_auth).unwrap(),
    ));
    for candidate in candidates {
        let store = Arc::new(Store {
            client: fixture.store.client.clone(),
            scope: fixture.store.scope.clone(),
            policy_digest: Some(candidate.digest().to_owned()),
            policy: Some(candidate),
            recovery_only: false,
        });
        assert!(
            SessionManager::restore(8, Duration::from_secs(3600), store, &capacity)
                .await
                .is_err()
        );
        assert_eq!(capacity.available_permits(), 1);
        // Failure precedes claims/boot; the original fenced writer stays valid.
        fixture.session.ledger.get().unwrap().renew().await.unwrap();
    }
    assert!(state.process_table.list_process_ids().await.is_empty());
    fixture.db.close().await;
}

#[tokio::test]
async fn pinned_credentials_do_not_follow_a_changed_daemon_account_selection() {
    let (home, state) = crate::test_support::server_state(8192, false).await;
    let mode = chaos_kern::auth::AuthCredentialsStoreMode::Ephemeral;
    chaos_kern::auth::login_with_api_key(home.path(), "sk-test-original-account-credential", mode)
        .unwrap();
    state.auth_manager.reload();
    let mut credentials = chaos_kern::auth::load_auth_dot_json(home.path(), mode)
        .unwrap()
        .unwrap();
    let mut sibling = credentials.provider_record("openai").unwrap();
    sibling.api_key = Some("sibling-original-key".into());
    credentials.set_provider_record("xai", sibling.clone());
    chaos_kern::auth::save_auth(home.path(), &credentials, mode).unwrap();
    let pin = PinnedPolicy::new(state.config.clone(), state.auth_manager.clone()).unwrap();
    let old_key = pin
        .auth()
        .auth_cached()
        .unwrap()
        .api_key()
        .unwrap()
        .to_owned();
    chaos_kern::auth::login_with_api_key(
        home.path(),
        "sk-test-new-account-credential",
        chaos_kern::auth::AuthCredentialsStoreMode::Ephemeral,
    )
    .unwrap();
    let mut changed = chaos_kern::auth::load_auth_dot_json(home.path(), mode)
        .unwrap()
        .unwrap();
    sibling.api_key = Some("sibling-changed-key".into());
    changed.set_provider_record("xai", sibling);
    chaos_kern::auth::save_auth(home.path(), &changed, mode).unwrap();
    // The reload signal tracks auth mode; identity can change within API-key mode.
    state.auth_manager.reload();
    assert_ne!(
        state.auth_manager.auth_cached().unwrap().api_key(),
        Some(old_key.as_str())
    );
    pin.auth().reload();
    assert_eq!(
        pin.auth().auth_cached().unwrap().api_key(),
        Some(old_key.as_str()),
        "existing retained executors must not start using another account"
    );
    assert_eq!(
        pin.auth()
            .for_provider("xai")
            .auth_cached()
            .unwrap()
            .api_key(),
        Some("sibling-original-key"),
        "delegated providers must use the same frozen credential sources"
    );
    pin.check().unwrap();
    let replacement = PinnedPolicy::new(state.config.clone(), state.auth_manager.clone()).unwrap();
    assert_ne!(
        pin.digest(),
        replacement.digest(),
        "a restart with the new account is policy drift"
    );
}

#[tokio::test]
async fn changed_role_material_before_dispatch_contains_without_submitting() {
    let (_home, state) = crate::test_support::server_state(8192, false).await;
    let path = state.config.chaos_home.join("role.toml");
    std::fs::write(&path, "model = 'first'\n").unwrap();
    let mut config = state.config.as_ref().clone();
    config.agent_roles.insert(
        "fixed".into(),
        chaos_kern::config::AgentRoleConfig {
            source: Some(chaos_kern::config::AgentRoleSource::File(path.clone())),
            ..Default::default()
        },
    );
    let pin = Arc::new(PinnedPolicy::new(Arc::new(config), state.auth_manager.clone()).unwrap());
    let mut fixture = Fixture::with_policy(Some(pin)).await;
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    std::fs::write(path, "model = 'changed'\n").unwrap();
    let mut supervisor = fixture.start();
    assert!(
        matches!(operation(&mut supervisor).await, Operation::Shutdown),
        "drift between admission and dispatch cannot submit a prompt"
    );
    status(&fixture.session, SessionStatus::Unknown).await;
    assert_eq!(fixture.capacity.available_permits(), 0);
    assert!(supervisor.operations.try_recv().is_err());
    supervisor.task.abort();
    let _ = supervisor.task.await;
    fixture.db.close().await;
}

#[tokio::test]
async fn recovery_only_never_boots_history_or_exposes_it_to_an_ordinary_caller() {
    let fixture = Fixture::new().await;
    // An orderly idle handoff would normally authorize lazy resume.
    fixture
        .session
        .ledger
        .get()
        .unwrap()
        .retire(true)
        .await
        .unwrap();
    let capacity = Arc::new(Semaphore::new(1));
    let store = store_like(&fixture, fixture.store.policy_digest.clone(), true);
    let (manager, recovered) =
        SessionManager::restore(8, Duration::from_secs(3600), store, &capacity)
            .await
            .unwrap();
    assert!(recovered.is_empty());
    let manager = Arc::new(manager);
    let session = manager.get(&fixture.session.id).unwrap();
    assert_eq!(session.snapshot().status, SessionStatus::Unknown);
    assert_eq!(capacity.available_permits(), 0);
    let (_home, mut state) = crate::test_support::server_state(8192, false).await;
    let unique = Arc::get_mut(&mut state).unwrap();
    unique.sessions = Some(manager.clone());
    unique.recovery_token = Some(Arc::from("recovery-test-token"));
    unique.semaphore = capacity.clone();
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for (operator, method, path, expected) in [
        (
            false,
            "GET",
            format!("/api/sessions/{}", session.id),
            StatusCode::FORBIDDEN,
        ),
        (true, "POST", "/api/trigger".into(), StatusCode::CONFLICT),
        (true, "POST", "/api/sessions".into(), StatusCode::CONFLICT),
        (
            true,
            "POST",
            format!("/api/sessions/{}/turns/{}", session.id, Uuid::new_v4()),
            StatusCode::CONFLICT,
        ),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", "Bearer test-token")
            .header("x-chaos-daemon-id", &manager.daemon_id)
            .header("content-type", "application/json");
        if operator {
            request = request.header(
                "x-chaos-recovery-authorization",
                "Bearer recovery-test-token",
            );
        }
        let response = crate::api::http_service(state.clone())
            .serve(
                request
                    .body(crate::test_support::counting_body(&polls))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let view = serde_json::to_value(session.recovery_view().await.unwrap()).unwrap();
    let resolution =
        serde_json::from_value::<crate::sessions::durable::recovery::ResolveRequest>(json!({
            "resolution_id": Uuid::new_v4(), "expected_process_id": session.snapshot().process_id,
            "expected_turn_id": null, "expected_recovery_epoch": view["session"]["recovery_epoch"],
            "containment_confirmed": true, "actor": "operator", "reason": "independently contained",
        }))
        .unwrap();
    session.resolve(resolution).await.unwrap();
    assert_eq!(capacity.available_permits(), 1);
    assert!(state.process_table.list_process_ids().await.is_empty());
    assert!(
        session
            .admit(
                Uuid::new_v4().to_string(),
                request("fresh"),
                &capacity,
                Duration::from_secs(60)
            )
            .await
            .is_err()
    );
    fixture.db.close().await;
}

#[tokio::test]
async fn legacy_scope_requires_recovery_instead_of_unverified_policy_adoption() {
    let fixture = Fixture::new().await;
    let legacy = "legacy-scope";
    let id = Uuid::new_v4();
    fixture
        .store
        .call(ExecutionRequest::Claim {
            session_id: id,
            scope: legacy.into(),
            owner_id: "old-daemon".into(),
            ttl_ms: 30_000,
        })
        .await
        .unwrap();
    fixture.db.expire(&id.to_string()).await;
    let normal = Arc::new(Store {
        client: fixture.store.client.clone(),
        scope: legacy.into(),
        policy_digest: Some("b".repeat(64)),
        policy: None,
        recovery_only: false,
    });
    let capacity = Arc::new(Semaphore::new(1));
    assert!(
        SessionManager::restore(8, Duration::from_secs(3600), normal, &capacity)
            .await
            .is_err()
    );
    let recovery = Arc::new(Store {
        client: fixture.store.client.clone(),
        scope: legacy.into(),
        policy_digest: None,
        policy: None,
        recovery_only: true,
    });
    let (manager, recovered) =
        SessionManager::restore(8, Duration::from_secs(3600), recovery, &capacity)
            .await
            .unwrap();
    assert!(recovered.is_empty());
    assert_eq!(
        manager.get(&id.to_string()).unwrap().snapshot().status,
        SessionStatus::Closed
    );
    assert_eq!(capacity.available_permits(), 1);
    fixture.db.close().await;
}
