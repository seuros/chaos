use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use rama::http::Body;
use rama::http::Request;
use rama::http::StatusCode;
use rama::http::body::util::BodyExt;
use tempfile::tempdir;

use super::JOURNAL_RPC_PATH;
use super::JournalRpcServer;
use super::PROTOCOL_VERSION;
use super::SERVER_NAME;
use super::SERVER_VERSION;
use crate::CreateProcessInput;
use crate::HelloRequest;
use crate::JournalRequest;
use crate::JournalResponse;
use crate::RequestEnvelope;
use crate::ResponseEnvelope;
use crate::SqliteJournalStore;

#[tokio::test]
async fn hello_round_trip_over_http() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let store = SqliteJournalStore::open(&temp_dir.path().join("journal.sqlite"))
        .await
        .unwrap_or_else(|err| panic!("open: {err}"));
    let server = JournalRpcServer::new(Arc::new(store), "sqlite");
    let request = Request::builder()
        .method("POST")
        .uri(JOURNAL_RPC_PATH)
        .body(Body::from(
            serde_json::to_vec(&RequestEnvelope {
                id: "1".to_string(),
                request: JournalRequest::Hello(HelloRequest {
                    client_name: "test".to_string(),
                    protocol_version: 1,
                }),
            })
            .unwrap_or_else(|err| panic!("serialize request: {err}")),
        ))
        .unwrap_or_else(|err| panic!("build request: {err}"));

    let response = server.handle_http(request).await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .unwrap_or_else(|err| panic!("collect body: {err}"))
        .to_bytes();
    let envelope: ResponseEnvelope =
        serde_json::from_slice(&body).unwrap_or_else(|err| panic!("deserialize response: {err}"));
    assert!(envelope.ok);
    match envelope.result {
        Some(JournalResponse::Hello(hello)) => {
            assert_eq!(hello.server_name, SERVER_NAME);
            assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
            assert_eq!(hello.server_version, SERVER_VERSION);
            assert_eq!(hello.backend, "sqlite");
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn create_process_round_trip_over_http() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let store = SqliteJournalStore::open(&temp_dir.path().join("journal.sqlite"))
        .await
        .unwrap_or_else(|err| panic!("open: {err}"));
    let server = JournalRpcServer::new(Arc::new(store), "sqlite");
    let process_id = ProcessId::new();
    let request = Request::builder()
        .method("POST")
        .uri(JOURNAL_RPC_PATH)
        .body(Body::from(
            serde_json::to_vec(&RequestEnvelope {
                id: "create-1".to_string(),
                request: JournalRequest::CreateProcess(CreateProcessInput {
                    process_id,
                    parent: None,
                    source: SessionSource::Cli,
                    cwd: temp_dir.path().to_path_buf(),
                    created_at: jiff::Timestamp::now(),
                    title: Some("rpc process".to_string()),
                    model_provider: Some("openai".to_string()),
                    cli_version: Some("47.0.0".to_string()),
                }),
            })
            .unwrap_or_else(|err| panic!("serialize request: {err}")),
        ))
        .unwrap_or_else(|err| panic!("build request: {err}"));

    let response = server.handle_http(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap_or_else(|err| panic!("collect body: {err}"))
        .to_bytes();
    let envelope: ResponseEnvelope =
        serde_json::from_slice(&body).unwrap_or_else(|err| panic!("deserialize response: {err}"));
    assert!(envelope.ok);
    match envelope.result {
        Some(JournalResponse::CreateProcess(result)) => {
            assert_eq!(result.process_id, process_id);
            assert_eq!(result.next_seq, 0);
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn initialize_process_round_trip_over_http() {
    use crate::InitializeProcessInput;
    use crate::JournalEntry;
    use crate::LoadJournalRequest;
    use chaos_ipc::protocol::CompactedItem;
    use chaos_ipc::protocol::RolloutItem;
    use chaos_ipc::protocol::SessionMeta;
    use chaos_ipc::protocol::SessionMetaLine;
    use chaos_ipc::protocol::SessionSource;

    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let store = SqliteJournalStore::open(&temp_dir.path().join("journal.sqlite"))
        .await
        .unwrap_or_else(|err| panic!("open: {err}"));
    let server = JournalRpcServer::new(Arc::new(store), "sqlite");
    let process_id = ProcessId::new();
    let now = jiff::Timestamp::now();

    // Simulate the recorder's first-batch shape: SessionMeta followed by a
    // transcript-bearing item. The atomic op must persist both — or neither —
    // so resume can rely on the no-header-only invariant.
    let session_meta_item = RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            id: process_id,
            forked_from_id: None,
            timestamp: now.to_string(),
            cwd: temp_dir.path().to_path_buf(),
            originator: "test".to_string(),
            cli_version: "47.0.0".to_string(),
            source: SessionSource::Cli,
            agent_nickname: None,
            agent_role: None,
            model_provider: Some("openai".to_string()),
            base_instructions: None,
            dynamic_tools: None,
            memory_mode: None,
        },
        git: None,
    });
    let transcript_item = RolloutItem::Compacted(CompactedItem {
        message: "opening turn".to_string(),
        replacement_history: None,
    });

    let request_body = serde_json::to_vec(&RequestEnvelope {
        id: "init-1".to_string(),
        request: JournalRequest::InitializeProcess(InitializeProcessInput {
            create: CreateProcessInput {
                process_id,
                parent: None,
                source: SessionSource::Cli,
                cwd: temp_dir.path().to_path_buf(),
                created_at: now,
                title: Some("init-rpc".to_string()),
                model_provider: Some("openai".to_string()),
                cli_version: Some("47.0.0".to_string()),
            },
            owner_id: "owner-rpc".to_string(),
            ttl_ms: 30_000,
            items: vec![
                JournalEntry {
                    seq: 0,
                    recorded_at: now,
                    item: session_meta_item.clone(),
                },
                JournalEntry {
                    seq: 1,
                    recorded_at: now,
                    item: transcript_item.clone(),
                },
            ],
        }),
    })
    .unwrap_or_else(|err| panic!("serialize request: {err}"));

    let request = Request::builder()
        .method("POST")
        .uri(JOURNAL_RPC_PATH)
        .body(Body::from(request_body))
        .unwrap_or_else(|err| panic!("build request: {err}"));

    let response = server.handle_http(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap_or_else(|err| panic!("collect body: {err}"))
        .to_bytes();
    let envelope: ResponseEnvelope =
        serde_json::from_slice(&body).unwrap_or_else(|err| panic!("deserialize response: {err}"));
    assert!(envelope.ok, "envelope error: {:?}", envelope.error);
    let init_result = match envelope.result {
        Some(JournalResponse::InitializeProcess(result)) => *result,
        other => panic!("unexpected result: {other:?}"),
    };
    assert_eq!(init_result.process.process_id, process_id);
    assert_eq!(init_result.next_seq, 2);
    assert_eq!(init_result.lease.owner_id, "owner-rpc");
    assert!(!init_result.lease.lease_token.is_empty());

    // Verify the journal landed both items in one shot, in order.
    let load_request = serde_json::to_vec(&RequestEnvelope {
        id: "load-1".to_string(),
        request: JournalRequest::LoadJournal(LoadJournalRequest { process_id }),
    })
    .unwrap_or_else(|err| panic!("serialize load: {err}"));
    let load_response = server
        .handle_http(
            Request::builder()
                .method("POST")
                .uri(JOURNAL_RPC_PATH)
                .body(Body::from(load_request))
                .unwrap_or_else(|err| panic!("build load request: {err}")),
        )
        .await;
    let load_body = load_response
        .into_body()
        .collect()
        .await
        .unwrap_or_else(|err| panic!("collect load body: {err}"))
        .to_bytes();
    let load_envelope: ResponseEnvelope = serde_json::from_slice(&load_body)
        .unwrap_or_else(|err| panic!("deserialize load response: {err}"));
    assert!(load_envelope.ok);
    let loaded = match load_envelope.result {
        Some(JournalResponse::LoadJournal(loaded)) => loaded,
        other => panic!("unexpected load result: {other:?}"),
    };
    assert_eq!(loaded.items.len(), 2);
    assert!(matches!(loaded.items[0].item, RolloutItem::SessionMeta(_)));
    assert!(matches!(loaded.items[1].item, RolloutItem::Compacted(_)));
}
