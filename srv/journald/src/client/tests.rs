use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use rama::http::server::HttpServer;
use rama::rt::Executor;
use tempfile::tempdir;

use super::JournalRpcClient;
use crate::CreateProcessInput;
use crate::JournalRpcServer;
use crate::SqliteJournalStore;

#[tokio::test]
async fn client_round_trip_over_unix_socket() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let socket_path = temp_dir.path().join("journald.sock");
    let db_path = temp_dir.path().join("journal.sqlite");
    let store = Arc::new(
        SqliteJournalStore::open(&db_path)
            .await
            .unwrap_or_else(|err| panic!("open store: {err}")),
    );
    let service = JournalRpcServer::new(store, "sqlite").http_service();
    let socket_path_for_server = socket_path.clone();
    let server_task = tokio::spawn(async move {
        HttpServer::new_http1(Executor::default())
            .listen_unix(&socket_path_for_server, service)
            .await
            .unwrap_or_else(|err| panic!("listen_unix: {err}"));
    });

    for _ in 0..50 {
        if tokio::fs::metadata(&socket_path).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        tokio::fs::metadata(&socket_path).await.is_ok(),
        "socket did not appear"
    );

    let client = JournalRpcClient::new(socket_path.clone());
    let hello = client
        .hello("journal-client-test")
        .await
        .unwrap_or_else(|err| panic!("hello: {err}"));
    assert!(crate::hello_is_compatible(&hello));

    let process_id = ProcessId::new();
    let created = client
        .create_process(CreateProcessInput {
            process_id,
            parent: None,
            source: SessionSource::Cli,
            cwd: temp_dir.path().to_path_buf(),
            created_at: jiff::Timestamp::now(),
            title: Some("client round trip".to_string()),
            model_provider: Some("openai".to_string()),
            cli_version: Some("47.0.0".to_string()),
        })
        .await
        .unwrap_or_else(|err| panic!("create_process: {err}"));
    assert_eq!(created.process_id, process_id);
    assert_eq!(created.next_seq, 0);

    server_task.abort();
    let _ = server_task.await;
    let _ = tokio::fs::remove_file(&socket_path).await;
}
