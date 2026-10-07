use super::*;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chaos_recall::test_support::{Embedder, Store};
use tokio::sync::Mutex;

fn request(name: &str, value: Value) -> Request {
    Request::parse(name, &value.to_string(), true).unwrap()
}

async fn call(
    service: &RecallService<Store, Embedder>,
    name: &str,
    value: Value,
    scope: RecallScope,
) -> Value {
    let output = execute(
        service,
        request(name, value),
        scope,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    serde_json::from_str(&output).unwrap()
}

#[test]
fn arguments_are_strict_bounded_and_global_writes_are_rejected() {
    for (name, args) in [
        (SEARCH, json!({"query": "x", "project_id": "other"})),
        (
            SEARCH,
            json!({"query": "x", "scope": {"kind": "project", "id": "other"}}),
        ),
        (SEARCH, json!({"query": "x", "limit": 0})),
        (SEARCH, json!({"query": "x", "limit": 51})),
        (SEARCH, json!({"query": "x", "limit": 1.5})),
        (SEARCH, json!({"query": "é".repeat(2049)})),
        (SEARCH, json!({"query": " "})),
        (
            "recall_store",
            json!({"id": "x", "content": "x", "scope": "global"}),
        ),
        ("recall_delete", json!({"id": "x", "scope": "global"})),
        ("recall_delete", json!({"id": "x", "session_id": "other"})),
        ("recall_delete", json!({"id": "x".repeat(257)})),
        (
            "recall_store",
            json!({"id": "x", "content": "x".repeat(16_385)}),
        ),
        (
            "recall_store",
            json!({"id": "x", "content": "x", "metadata": {"v": "x".repeat(8192)}}),
        ),
        (
            "recall_store",
            json!({"id": "x", "content": "x", "embedding": [1]}),
        ),
    ] {
        assert!(
            Request::parse(name, &args.to_string(), true).is_err(),
            "{name}: {args}"
        );
    }
    assert!(
        Request::parse(
            SEARCH,
            &json!({"query": "é".repeat(2048)}).to_string(),
            false
        )
        .is_ok()
    );
    assert!(Request::parse("recall_store", r#"{"id":"x","content":"x"}"#, false).is_err());
    assert!(Request::parse("recall_delete", r#"{"id":"x"}"#, false).is_err());
    let Request::Search(args) = request(SEARCH, json!({"query": "x"})) else {
        panic!()
    };
    assert!(matches!(args.scope, Scope::Project));
    assert_eq!(args.limit, 10);
    assert!(!args.include_global);
}

#[tokio::test]
async fn receipts_only_authorize_displayed_matches_and_explicit_use_is_idempotent() {
    let store = Store::default();
    let service = RecallService::new(store.clone(), Embedder, Duration::from_secs(5));
    let scope = RecallScope::Session("session".into());
    for index in 0..50 {
        call(
            &service,
            "recall_store",
            json!({
                "id": format!("{index:02}"), "content": format!("memory{}", "\n".repeat(8000)),
                "metadata": {"large": "x".repeat(7000)}, "memory": {"automatic": true},
            }),
            scope.clone(),
        )
        .await;
    }
    let response = call(
        &service,
        SEARCH,
        json!({"query": "query", "limit": 50}),
        scope.clone(),
    )
    .await;
    assert!(response["omitted_matches"].as_u64().unwrap() > 0);
    let receipt = response["receipt"]["id"].as_str().unwrap();
    let matches = response["matches"].as_array().unwrap();
    let id = matches[0]["id"].as_str().unwrap();
    let arguments = json!({"receipt": receipt, "id": id});
    let used = call(&service, "recall_use", arguments.clone(), scope.clone()).await;
    assert_eq!(used["applied"], true);
    assert_eq!(used["charge"], 0.52);
    let replay = call(&service, "recall_use", arguments, scope.clone()).await;
    assert_eq!(replay["applied"], false);
    let omitted = (0..50)
        .map(|index| format!("{index:02}"))
        .find(|id| {
            !matches
                .iter()
                .any(|item| item["id"].as_str() == Some(id.as_str()))
        })
        .unwrap();
    assert!(
        execute(
            &service,
            request("recall_use", json!({"receipt": receipt, "id": omitted})),
            scope,
            CancellationToken::new()
        )
        .await
        .is_err()
    );
}

#[test]
fn open_is_read_only_and_use_arguments_are_strict() {
    let receipt = uuid::Uuid::new_v4().to_string();
    let args = json!({"receipt": receipt, "id": "id", "scope": "global"});
    assert!(Request::parse("recall_open", &args.to_string(), false).is_ok());
    assert!(Request::parse("recall_use", &args.to_string(), true).is_err());
    assert!(
        Request::parse(
            "recall_use",
            &json!({"receipt": receipt, "id": "id"}).to_string(),
            false
        )
        .is_err()
    );
    for args in [
        json!({"receipt": "invalid", "id": "id"}),
        json!({"receipt": receipt, "id": "id", "source_index": 0}),
        json!({"receipt": receipt, "id": "id", "session_id": "other"}),
    ] {
        assert!(Request::parse("recall_use", &args.to_string(), true).is_err());
    }
}

#[tokio::test]
async fn store_search_delete_preserve_provenance_and_namespace_isolation() {
    let store = Store::default();
    let service = RecallService::new(store.clone(), Embedder, Duration::from_secs(1));
    let project = RecallScope::Project("trusted-project".into());
    let scopes = [
        project.clone(),
        RecallScope::Project("other-project".into()),
        RecallScope::Session("trusted-session".into()),
        RecallScope::Session("other-session".into()),
        RecallScope::Global,
    ];
    let metadata = json!({"source": "\"verbatim\"", "nested": [1, true]});
    for scope in &scopes {
        service
            .store(
                scope.clone(),
                "same-id".into(),
                "remember this".into(),
                metadata.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    let stored = call(
        &service,
        "recall_store",
        json!({"id": "same-id", "content": "updated", "metadata": metadata}),
        project.clone(),
    )
    .await;
    assert_eq!(
        stored["scope"],
        json!({"kind": "project", "id": "trusted-project"})
    );
    assert_eq!(stored["id"], "same-id");
    for scope in &scopes {
        let result = call(&service, SEARCH, json!({"query": "memory"}), scope.clone()).await;
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(
            result["matches"][0]["scope"],
            serde_json::to_value(scope).unwrap()
        );
        assert_eq!(result["matches"][0]["metadata"], metadata);
        assert_eq!(
            result["matches"][0]["methods"],
            json!(["semantic", "lexical"])
        );
    }
    let global = call(
        &service,
        SEARCH,
        json!({"query": "memory", "include_global": true}),
        project.clone(),
    )
    .await;
    assert_eq!(global["matches"].as_array().unwrap().len(), 2);
    let deleted = call(
        &service,
        "recall_delete",
        json!({"id": "same-id"}),
        project.clone(),
    )
    .await;
    assert_eq!(deleted["scope"], stored["scope"]);
    assert!(
        call(
            &service,
            SEARCH,
            json!({"query": "memory"}),
            project.clone()
        )
        .await["matches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.docs.lock().await.len(), 4);
    assert!(
        execute(
            &service,
            request("recall_delete", json!({"id": "same-id"})),
            RecallScope::Global,
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(
        execute(
            &service,
            request("recall_store", json!({"id": "x", "content": "x"})),
            RecallScope::Global,
            CancellationToken::new()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn degraded_results_keep_warnings_without_private_diagnostics() {
    let store = Store::default();
    store.fail_semantic.store(true, Ordering::SeqCst);
    let service = RecallService::new(store, Embedder, Duration::from_secs(1));
    let result = call(
        &service,
        SEARCH,
        json!({"query": "x"}),
        RecallScope::Session("s".into()),
    )
    .await;
    assert_eq!(result["warnings"][0]["method"], "semantic");
    assert!(!result.to_string().contains("private SQL"));
}

#[tokio::test]
async fn dropping_tool_search_aborts_retrieval_branches() {
    let store = Store::default();
    store.stalled.store(true, Ordering::SeqCst);
    let service = Arc::new(RecallService::new(
        store.clone(),
        Embedder,
        Duration::from_secs(30),
    ));
    let search = tokio::spawn(async move {
        execute(
            service.as_ref(),
            request(SEARCH, json!({"query": "x"})),
            RecallScope::Session("s".into()),
            CancellationToken::new(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.active.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    search.abort();
    let _ = search.await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn output_is_utf8_safe_and_bounded_after_json_escaping() {
    let response = RecallResponse {
        matches: (0..50)
            .map(|index| chaos_recall::RecallMatch {
                scope: RecallScope::Session("s".into()),
                id: index.to_string(),
                content: format!("{}{}", "🦀".repeat(1023), "é".repeat(10)),
                metadata: json!({"escaped": "\n".repeat(9000)}),
                rank_score: 1.0,
                methods: vec![chaos_recall::RetrievalMethod::Semantic],
                handle: Default::default(),
            })
            .collect(),
        warnings: vec![],
        receipt: None,
    };
    let output = search_output(RecallScope::Session("s".into()), response).unwrap();
    assert!(output.len() <= OUTPUT_BYTES);
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["truncated"], true);
    assert!(value["omitted_matches"].as_u64().unwrap() > 0);
    for result in value["matches"].as_array().unwrap() {
        assert!(result["content"].as_str().unwrap().len() <= SNIPPET_BYTES);
        assert_eq!(result["content_truncated"], true);
        assert_eq!(result["metadata_omitted"], true);
        assert_eq!(result["metadata"], Value::Null);
        assert!(result["id"].is_string());
        assert!(result["rank_score"].is_number());
    }
    let mut multibyte = "🦀".repeat(1025);
    assert!(truncate_utf8(&mut multibyte, 4095));
    assert_eq!(multibyte.len(), 4092);
}

#[tokio::test]
async fn project_scope_uses_most_specific_checkout_and_installation() {
    let home = tempfile::tempdir().unwrap();
    let nested = home.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let db = RuntimeDbHandle::Sqlite(
        chaos_proc::StateRuntime::init(home.path().into(), "test".into())
            .await
            .unwrap(),
    );
    let workspace = db.planning_create_workspace("recall-tests").await.unwrap();
    let outer = db
        .planning_register_project(&workspace.id, "outer")
        .await
        .unwrap();
    let inner = db
        .planning_register_project(&workspace.id, "inner")
        .await
        .unwrap();
    db.planning_bind("installation", &outer.id, home.path())
        .await
        .unwrap();
    db.planning_bind("installation", &inner.id, &nested)
        .await
        .unwrap();
    assert_eq!(
        project_scope(Some(&db), "installation", &nested)
            .await
            .unwrap(),
        RecallScope::Project(inner.id)
    );
    assert_eq!(
        project_scope(Some(&db), "installation", home.path())
            .await
            .unwrap(),
        RecallScope::Project(outer.id)
    );
    assert!(
        project_scope(Some(&db), "other-installation", &nested)
            .await
            .is_err()
    );
    assert!(
        project_scope(None, "installation", &nested)
            .await
            .unwrap_err()
            .to_string()
            .contains("scope=\"session\"")
    );
}

#[tokio::test]
async fn forced_handler_mutation_is_denied_without_ready_service() {
    let (session, mut turn) = crate::chaos::make_session_and_context().await;
    turn.mode_capabilities.mutation = false;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    for name in ["recall_store", "recall_delete", "recall_use"] {
        let invocation = ToolInvocation {
            session: session.clone(),
            turn: turn.clone(),
            tracker: Arc::new(Mutex::new(
                crate::turn_diff_tracker::TurnDiffTracker::default(),
            )),
            call_id: "forced".into(),
            tool_name: name.into(),
            tool_namespace: None,
            payload: crate::tools::context::ToolPayload::Function {
                arguments: r#"{"id":"x","content":"x"}"#.into(),
            },
        };
        assert!(Handler.is_mutating(&invocation).await);
        assert!(
            Handler
                .handle(invocation)
                .await
                .err()
                .expect("forced mutation must fail")
                .to_string()
                .contains("read-only")
        );
    }
}
