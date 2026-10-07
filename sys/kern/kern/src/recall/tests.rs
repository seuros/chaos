use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn postgres() -> Vfs {
    Vfs::Postgres(sqlx::PgPool::connect_lazy("postgres://localhost/unused").unwrap())
}

#[tokio::test]
async fn disabled_mounts_do_not_create_runtime_and_postgres_startup_is_lazy() {
    let cache = OnceLock::new();
    let sqlite = Vfs::Sqlite(sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap());
    for mount in [None, Some(sqlite)] {
        assert!(for_mount(mount, PathBuf::from("invalid"), &cache).is_none());
        assert!(cache.get().is_none());
    }
    let home = tempfile::tempdir().unwrap();
    let first = for_mount(Some(postgres()), home.path().to_owned(), &cache).unwrap();
    assert!(first.ready_service().is_none());
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    let second = for_mount(Some(postgres()), PathBuf::from("/unused"), &cache).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
}

pub(super) fn failing_runtime(calls: Arc<AtomicUsize>) -> Arc<RecallRuntime> {
    Arc::new(RecallRuntime::new(Arc::new(move |_, _| {
        let calls = calls.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Err("test preparation failure".into())
        })
    })))
}

async fn group_call(
    session: Arc<crate::chaos::Session>,
    turn: Arc<crate::chaos::TurnContext>,
    name: &str,
    groups: serde_json::Value,
) -> Result<crate::tools::context::FunctionToolOutput, crate::function_tool::FunctionCallError> {
    use crate::tools::context::{ToolInvocation, ToolPayload};
    use crate::tools::registry::ToolHandler;
    let invocation = ToolInvocation {
        session,
        turn,
        tracker: Arc::new(tokio::sync::Mutex::new(
            crate::turn_diff_tracker::TurnDiffTracker::default(),
        )),
        call_id: "recall-activation-test".into(),
        tool_name: name.into(),
        tool_namespace: None,
        payload: ToolPayload::Function {
            arguments: serde_json::json!({"groups": groups}).to_string(),
        },
    };
    crate::tools::handlers::ToolGroupsHandler
        .handle(invocation)
        .await
}

#[tokio::test]
async fn preflight_and_failed_mixed_batch_are_atomic_disable_is_lazy_and_retry_is_explicit() {
    use serde_json::json;
    let (mut session, turn) = crate::chaos::make_session_and_context().await;
    let calls = Arc::new(AtomicUsize::new(0));
    session.services.recall = Some(failing_runtime(calls.clone()));
    session
        .services
        .tool_group_catalog
        .set_groups_enabled(&session.services.tool_group_state, ["recall", "web"], false)
        .unwrap();
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    assert!(
        group_call(
            session.clone(),
            turn.clone(),
            "enable_tools",
            json!(["recall", "unknown"])
        )
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("unknown")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        group_call(
            session.clone(),
            turn.clone(),
            "enable_tools",
            json!(["web", "recall"])
        )
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("test preparation failure")
    );
    for group in ["web", "recall"] {
        assert!(
            !session
                .services
                .tool_group_catalog
                .is_group_enabled(&session.services.tool_group_state, group)
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    group_call(
        session.clone(),
        turn.clone(),
        "disable_tools",
        json!(["recall"]),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        group_call(session, turn, "enable_tools", json!(["recall"]))
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn preenabled_failure_disables_only_recall_and_does_not_retry_each_turn() {
    let (mut session, turn) = crate::chaos::make_session_and_context().await;
    let calls = Arc::new(AtomicUsize::new(0));
    session.services.recall = Some(failing_runtime(calls.clone()));
    session
        .services
        .tool_group_catalog
        .set_groups_enabled(&session.services.tool_group_state, ["web", "recall"], true)
        .unwrap();
    assert!(prepare_for_router(&session, &turn).await.is_some());
    assert!(
        !session
            .services
            .tool_group_catalog
            .is_group_enabled(&session.services.tool_group_state, "recall")
    );
    assert!(
        session
            .services
            .tool_group_catalog
            .is_group_enabled(&session.services.tool_group_state, "web")
    );
    assert!(prepare_for_router(&session, &turn).await.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_backend_fails_activation_without_initialization() {
    let (mut session, turn) = crate::chaos::make_session_and_context().await;
    session.services.recall = None;
    assert!(
        prepare(&session, &turn)
            .await
            .unwrap_err()
            .contains("PostgreSQL")
    );
}

#[tokio::test]
async fn network_grant_allows_recall_through_egress_without_changing_base_policy() {
    use chaos_ipc::models::{NetworkPermissions, PermissionProfile};
    use chaos_ipc::permissions::SocketPolicy;
    use chaos_ipc::protocol::PermissionUpdateScope;
    use std::sync::atomic::AtomicBool;
    let (mut session, mut turn) = crate::chaos::make_session_and_context().await;
    let config = Arc::make_mut(&mut turn.config);
    config.egress_url = Some("https://egress.invalid".into());
    config.permissions.socket_policy = SocketPolicy::Restricted;
    turn.socket_policy = SocketPolicy::Restricted;
    session.permission_actor.register_turn(&turn).await.unwrap();
    let routed = Arc::new(AtomicBool::new(false));
    let observed = routed.clone();
    session.services.recall = Some(Arc::new(RecallRuntime::new(Arc::new(move |network, _| {
        observed.store(
            network
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .is_some_and(|egress| egress.endpoint() == "https://egress.invalid"),
            Ordering::SeqCst,
        );
        Box::pin(async move {
            let _ = network?;
            Err("injected download failure".into())
        })
    }))));
    assert!(
        prepare(&session, &turn)
            .await
            .unwrap_err()
            .contains("sockets are disabled")
    );
    assert!(!routed.load(Ordering::SeqCst));
    session
        .permission_actor
        .merge_grant(
            PermissionUpdateScope::ActiveTurn {
                turn_id: turn.sub_id.clone(),
            },
            PermissionProfile {
                network: Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        prepare(&session, &turn)
            .await
            .unwrap_err()
            .contains("injected download failure")
    );
    assert!(routed.load(Ordering::SeqCst));
    assert_eq!(
        turn.config.permissions.socket_policy,
        SocketPolicy::Restricted
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL with pgvector and RECALL_MODEL_DIR local artifacts"]
async fn shared_model_and_native_tools_postgres_round_trip() {
    use crate::tools::context::{ToolInvocation, ToolPayload};
    use crate::tools::handlers::recall::Handler;
    use crate::tools::registry::ToolHandler;
    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    let url = std::env::var("TEST_DATABASE_URL").expect("set TEST_DATABASE_URL");
    let directory = PathBuf::from(std::env::var("RECALL_MODEL_DIR").expect("set RECALL_MODEL_DIR"));
    assert!(
        directory.is_absolute(),
        "test model directory must be absolute"
    );
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public")
        .execute(&admin)
        .await
        .unwrap();
    let schema = format!("kernel_recall_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = format!("{schema},public");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let search_path = search_path.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('search_path', $1, false)")
                    .bind(search_path)
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    // Cleanup also runs when assertions fail; this test never mounts a global VFS.
    let outcome = tokio::spawn({
        let pool = pool.clone();
        async move {
            let runtime = Arc::new(RecallRuntime::new(Arc::new(move |_, cancellation| {
                let pool = pool.clone();
                let directory = directory.clone();
                Box::pin(async move {
                    match RecallState::from_mount(Some(Vfs::Postgres(pool)), RecallConfig::new(directory),
                        cancellation).await.map_err(|err| err.to_string())? {
                        RecallState::Ready(service) => Ok(Arc::new(service)),
                        RecallState::Disabled => Err("test initialization disabled".into()),
                    }
                })
            })));
            let first = runtime.prepare(Err("offline test".into())).await.unwrap();
            let second = runtime.prepare(Err("offline test".into())).await.unwrap();
            assert!(Arc::ptr_eq(&first, &second));

            let (mut session, mut turn) = crate::chaos::make_session_and_context().await;
            session.services.recall = Some(runtime);
            turn.mode_capabilities.mutation = true;
            let scope = chaos_recall::RecallScope::Session(session.conversation_id.to_string());
            let session = Arc::new(session);
            let turn = Arc::new(turn);
            let call = |name: &'static str, arguments: Value| {
                let invocation = ToolInvocation {
                    session: session.clone(), turn: turn.clone(),
                    tracker: Arc::new(Mutex::new(crate::turn_diff_tracker::TurnDiffTracker::default())),
                    call_id: "recall-integration".into(), tool_name: name.into(),
                    tool_namespace: None,
                    payload: ToolPayload::Function { arguments: arguments.to_string() },
                };
                async move {
                    let output = Handler.handle(invocation).await.unwrap().into_text();
                    serde_json::from_str::<Value>(&output).unwrap()
                }
            };
            let stored = call("recall_store", json!({
                "scope": "session", "id": "build", "content": "Use Cargo to build the Rust project.",
                "metadata": {"source": "explicit user request"}
            })).await;
            assert_eq!(stored["scope"], serde_json::to_value(&scope).unwrap());
            // Same ID in another session and global must not enter default results.
            for namespace in [
                chaos_recall::RecallScope::Session("another-session".into()),
                chaos_recall::RecallScope::Global,
            ] {
                first.store(namespace, "build".into(), "Use Cargo to build.".into(), json!({}),
                    CancellationToken::new()).await.unwrap();
            }
            let result = call("recall_search", json!({"scope": "session", "query": "Cargo build"})).await;
            assert_eq!(result["matches"].as_array().unwrap().len(), 1);
            assert_eq!(result["matches"][0]["scope"], stored["scope"]);
            assert_eq!(result["matches"][0]["id"], "build");
            assert_eq!(result["matches"][0]["metadata"]["source"], "explicit user request");
            let expanded = call("recall_search", json!({
                "scope": "session", "query": "Cargo build", "include_global": true
            })).await;
            assert_eq!(expanded["matches"].as_array().unwrap().len(), 2);
            let deleted = call("recall_delete", json!({"scope": "session", "id": "build"})).await;
            assert_eq!(deleted["scope"], stored["scope"]);
            assert!(call("recall_search", json!({"scope": "session", "query": "Cargo build"}))
                .await["matches"].as_array().unwrap().is_empty());
        }
    }).await;
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    outcome.unwrap();
}
