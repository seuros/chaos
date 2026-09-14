use super::*;
use crate::catalog::Catalog;
use crate::catalog::CatalogSink;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_sysctl::Constrained;
use serde_json::json;
use tokio::time::Duration;
use tokio::time::timeout;

fn manager() -> McpConnectionManager {
    McpConnectionManager::new_uninitialized(&Constrained::allow_any(ApprovalPolicy::Interactive))
}

fn gate() -> Arc<McpCatalogGate> {
    Arc::new(McpCatalogGate::staging(Arc::new(CatalogSink::new(
        Catalog::from_inventory(),
    ))))
}

#[tokio::test]
async fn resource_subscription_state_tracks_transient_uris() {
    let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());

    actor.record_resource_subscription("coordinator", "agent://inbox", true);
    actor.record_resource_subscription("coordinator", "agent://inbox", true);
    actor.record_resource_subscription("coordinator", "agent://state", true);

    assert_eq!(
        resource_subscriptions_snapshot(&actor.resource_subscriptions),
        HashMap::from([(
            "coordinator".to_string(),
            vec!["agent://inbox".to_string(), "agent://state".to_string()],
        )])
    );

    actor.record_resource_subscription("coordinator", "agent://inbox", false);
    assert_eq!(
        resource_subscriptions_snapshot(&actor.resource_subscriptions),
        HashMap::from([("coordinator".to_string(), vec!["agent://state".to_string()],)])
    );

    actor.record_resource_subscription("coordinator", "agent://state", false);
    assert!(resource_subscriptions_snapshot(&actor.resource_subscriptions).is_empty());
    actor.shutdown().await.expect("shutdown registry");
}

#[tokio::test]
async fn client_identity_is_stable_scoped_and_replay_safe() {
    let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
    let first = actor.client_identities_for(["review-service".to_string()]);
    let second = actor.client_identities_for(["review-service".to_string(), "other".to_string()]);

    assert_eq!(first["review-service"], second["review-service"]);
    assert_ne!(second["review-service"], second["other"]);
    actor.shutdown().await.expect("shutdown registry");

    let conversation_id = chaos_ipc::ProcessId::new();
    let first_actor =
        McpRegistryActor::spawn_for_session(manager(), CancellationToken::new(), conversation_id);
    let first = first_actor.client_identities_for(["review-service".to_string()]);
    first_actor
        .shutdown()
        .await
        .expect("shutdown first registry");

    let replayed_actor =
        McpRegistryActor::spawn_for_session(manager(), CancellationToken::new(), conversation_id);
    let replayed =
        replayed_actor.client_identities_for(["review-service".to_string(), "other".to_string()]);

    assert_eq!(first["review-service"], replayed["review-service"]);
    assert_ne!(replayed["review-service"], replayed["other"]);
    replayed_actor
        .shutdown()
        .await
        .expect("shutdown replayed registry");

    let first_actor = McpRegistryActor::spawn_for_session(
        manager(),
        CancellationToken::new(),
        chaos_ipc::ProcessId::new(),
    );
    let second_actor = McpRegistryActor::spawn_for_session(
        manager(),
        CancellationToken::new(),
        chaos_ipc::ProcessId::new(),
    );

    let first = first_actor.client_identities_for(["review-service".to_string()]);
    let second = second_actor.client_identities_for(["review-service".to_string()]);

    assert_ne!(first["review-service"], second["review-service"]);
    first_actor
        .shutdown()
        .await
        .expect("shutdown first registry");
    second_actor
        .shutdown()
        .await
        .expect("shutdown second registry");
}

#[tokio::test]
async fn registry_shutdown_is_idempotent_and_stops_admissions() {
    let cancellation_token = CancellationToken::new();
    let actor = McpRegistryActor::spawn(manager(), cancellation_token.clone());

    actor.shutdown().await.expect("first registry shutdown");
    actor.shutdown().await.expect("second registry shutdown");

    assert!(cancellation_token.is_cancelled());
    let error = actor
        .execute("missing", |_, _| async { Ok(()) })
        .await
        .expect_err("shutdown registry must reject dispatch");
    assert!(error.to_string().contains("stopped"));
}

#[tokio::test]
async fn bootstrap_does_not_advance_revision() {
    let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
    let diff = actor
        .bootstrap(
            manager(),
            HashMap::new(),
            CancellationToken::new(),
            gate(),
            Vec::new(),
        )
        .await
        .expect("bootstrap");
    assert_eq!(diff.revision, 0);
    assert_eq!(actor.revision(), 0);
}

#[tokio::test]
async fn reconcile_advances_revision() {
    let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
    let diff = actor
        .reconcile(
            manager(),
            HashMap::new(),
            CancellationToken::new(),
            gate(),
            Vec::new(),
        )
        .await
        .expect("reconcile");
    assert_eq!(diff.revision, 1);
    assert_eq!(actor.revision(), 1);
}

#[tokio::test]
async fn disabled_configs_are_not_exposed_as_server_actors() {
    let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
    let disabled = serde_json::from_value(json!({
        "command": "disabled-server",
        "enabled": false
    }))
    .expect("disabled MCP config");
    actor
        .bootstrap(
            manager(),
            HashMap::from([("disabled".to_string(), disabled)]),
            CancellationToken::new(),
            gate(),
            Vec::new(),
        )
        .await
        .expect("bootstrap");

    assert!(!actor.has_servers());
    assert!(actor.server_names().is_empty());
}

#[tokio::test]
async fn refresh_actor_preserves_order_without_blocking_enqueue() {
    let actor = McpRefreshActor::spawn();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();

    let first_order_tx = order_tx.clone();
    actor
        .enqueue(async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            let _ = first_order_tx.send(1);
        })
        .await
        .expect("enqueue first refresh");
    actor
        .enqueue(async move {
            let _ = order_tx.send(2);
        })
        .await
        .expect("enqueue second refresh");

    started_rx.await.expect("first refresh started");
    assert!(
        timeout(Duration::from_millis(20), order_rx.recv())
            .await
            .is_err(),
        "the second refresh must remain queued while the first is running"
    );
    release_tx.send(()).expect("release first refresh");
    assert_eq!(order_rx.recv().await, Some(1));
    assert_eq!(order_rx.recv().await, Some(2));
    actor.shutdown().await.expect("shutdown refresh actor");
}

#[tokio::test]
async fn permission_sync_does_not_wait_for_running_server_call() {
    let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
    let enabled: McpServerConfig = serde_json::from_value(json!({
        "command": "enabled-server",
        "enabled": true
    }))
    .expect("enabled MCP config");
    actor
        .bootstrap(
            manager(),
            HashMap::from([("enabled".to_string(), enabled)]),
            CancellationToken::new(),
            gate(),
            Vec::new(),
        )
        .await
        .expect("bootstrap");

    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let executing_actor = actor.clone();
    let execution = tokio::spawn(async move {
        executing_actor
            .execute("enabled", move |_, _| async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Ok(())
            })
            .await
    });
    started_rx.await.expect("server call started");

    let sandbox_state = SandboxState {
        vfs_policy: chaos_ipc::permissions::VfsPolicy::default(),
        socket_policy: chaos_ipc::permissions::SocketPolicy::default(),
        alcatraz_exe: std::path::PathBuf::from("/alcatraz"),
        sandbox_cwd: std::path::PathBuf::from("/"),
    };
    timeout(
        Duration::from_millis(100),
        actor.sync_permission_state(
            Constrained::allow_any(ApprovalPolicy::Headless),
            sandbox_state,
        ),
    )
    .await
    .expect("permission sync should enqueue without waiting")
    .expect("permission sync");

    release_tx.send(()).expect("release server call");
    execution
        .await
        .expect("server call task")
        .expect("server call");
}

#[tokio::test]
async fn reconcile_installs_new_generation_and_awaits_bounded_retirement() {
    let actor = McpRegistryActor::spawn(manager(), CancellationToken::new());
    let enabled: McpServerConfig = serde_json::from_value(json!({
        "command": "enabled-server",
        "enabled": true
    }))
    .expect("enabled MCP config");
    actor
        .bootstrap(
            manager(),
            HashMap::from([("enabled".to_string(), enabled.clone())]),
            CancellationToken::new(),
            gate(),
            Vec::new(),
        )
        .await
        .expect("bootstrap");

    let old_manager = actor.current_manager();
    let old_cancellation_token = actor.cancellation_token().await;
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let executing_actor = actor.clone();
    let execution = tokio::spawn(async move {
        executing_actor
            .execute("enabled", move |manager, _| async move {
                let _ = started_tx.send(manager);
                let _ = release_rx.await;
                Ok(())
            })
            .await
    });
    let running_manager = started_rx.await.expect("server call started");
    assert!(Arc::ptr_eq(&old_manager, &running_manager));
    let old_manager_weak = Arc::downgrade(&old_manager);

    timeout(
        Duration::from_secs(3),
        actor.reconcile(
            manager(),
            HashMap::from([("enabled".to_string(), enabled)]),
            CancellationToken::new(),
            gate(),
            Vec::new(),
        ),
    )
    .await
    .expect("reconcile must bound old generation retirement")
    .expect("reconcile");

    let new_manager = actor.current_manager();
    assert!(!Arc::ptr_eq(&old_manager, &new_manager));
    let expected_manager = Arc::clone(&new_manager);
    let used_new_generation = timeout(
        Duration::from_millis(100),
        actor.execute("enabled", move |manager, _| async move {
            Ok(Arc::ptr_eq(&manager, &expected_manager))
        }),
    )
    .await
    .expect("new generation call should not wait for the old generation")
    .expect("new generation call");
    assert!(used_new_generation);
    assert!(
        old_cancellation_token.is_cancelled(),
        "retirement must cancel the old generation"
    );
    assert!(
        execution.is_finished(),
        "reconcile must not reply before the old server actor is retired"
    );
    drop(running_manager);
    drop(old_manager);

    assert!(
        release_tx.send(()).is_err(),
        "bounded retirement must abort an old call that refuses to drain"
    );
    execution
        .await
        .expect("old server call task")
        .expect_err("old server call must be interrupted during bounded retirement");
    timeout(Duration::from_secs(1), async {
        while old_manager_weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retired manager should be shut down and released after its calls drain");
}
