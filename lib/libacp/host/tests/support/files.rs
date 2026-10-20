use super::faults::{Peer, raw};
use super::*;
use serde_json::{Value, json};

async fn open(
    read: bool,
    write: bool,
    limits: ConnectionLimits,
) -> (
    Peer,
    JoinHandle<Result<(), HostError>>,
    Arc<Observations>,
    Value,
) {
    let (mut peer, task, observations) = raw(limits);
    peer.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":read,"writeTextFile":write}}
    }})).await;
    assert!(peer.receive().await["error"].is_null());
    peer.send(json!({"jsonrpc":"2.0","id":2,"method":"session/new",
        "params":{"cwd":"/workspace","mcpServers":[]}}))
        .await;
    let id = peer.receive().await["result"]["sessionId"].clone();
    (peer, task, observations, id)
}

#[tokio::test]
async fn file_requests_are_typed_session_scoped_and_require_exact_capabilities() {
    for (read, write, prompt, method) in [
        (true, false, "editor-read", "fs/read_text_file"),
        (true, true, "editor-write", "fs/write_text_file"),
    ] {
        let (mut peer, task, observations, id) =
            open(read, write, ConnectionLimits::default()).await;
        peer.prompt(3, &id, prompt).await;
        let request = peer.receive().await;
        assert_eq!(request["method"], method);
        assert_eq!(request["params"]["sessionId"], id);
        assert_eq!(request["params"]["path"], "/workspace/buffer.rs");
        let result = if prompt == "editor-read" {
            json!({"content":"unsaved marker"})
        } else {
            json!({})
        };
        peer.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
            .await;
        let mut text = String::new();
        loop {
            let response = peer.receive().await;
            if response["id"] == 3 {
                assert_eq!(response["result"]["stopReason"], "end_turn");
                break;
            }
            assert_eq!(response["method"], "session/update");
            if let Some(value) = response["params"]["update"]["content"]["text"].as_str() {
                text.push_str(value);
            }
        }
        if read && !write {
            assert!(text.contains("unsaved marker"));
        }
        let stale = observations.context.lock().unwrap().clone().unwrap();
        assert!(
            stale
                .client()
                .read_text_file(std::path::Path::new("/workspace/buffer.rs"))
                .await
                .is_err()
        );
        drop(peer);
        assert!(task.await.unwrap().is_ok());
    }
    for (read, write, prompt) in [(false, false, "editor-read"), (true, false, "editor-write")] {
        let (mut peer, task, _, id) = open(read, write, ConnectionLimits::default()).await;
        peer.prompt(3, &id, prompt).await;
        let response = peer.receive().await;
        assert_eq!(response["id"], 3);
        assert!(response["error"].is_object(), "{response}");
        drop(peer);
        assert!(task.await.unwrap().is_ok());
    }
}

#[tokio::test]
async fn cancelled_read_rejects_a_stale_reply_without_contaminating_next_turn() {
    let (mut peer, task, _, id) = open(true, true, ConnectionLimits::default()).await;
    peer.prompt(3, &id, "editor-read").await;
    let old = peer.receive().await;
    peer.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
        .await;
    assert_eq!(peer.receive().await["result"]["stopReason"], "cancelled");
    peer.send(json!({"jsonrpc":"2.0","id":old["id"],"result":{"content":"stale"}}))
        .await;
    peer.prompt(4, &id, "fresh").await;
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "2:fresh"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    drop(peer);
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn cancelled_unacknowledged_write_invalidates_session_even_after_late_success() {
    let (mut peer, task, _, id) = open(true, true, ConnectionLimits::default()).await;
    peer.prompt(3, &id, "editor-write").await;
    let old = peer.receive().await;
    peer.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
        .await;
    let unknown = peer.receive().await;
    assert!(unknown["error"].is_object(), "{unknown}");
    peer.send(json!({"jsonrpc":"2.0","id":old["id"],"result":{}}))
        .await;
    peer.prompt(4, &id, "fresh").await;
    assert!(peer.receive().await["error"].is_object());
    drop(peer);
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn unanswered_editor_request_has_a_finite_deadline_and_native_cleanup() {
    let (mut peer, task, observations, id) = open(
        true,
        true,
        ConnectionLimits {
            approval_timeout: Duration::from_millis(20),
            ..Default::default()
        },
    )
    .await;
    peer.prompt(3, &id, "editor-write").await;
    assert_eq!(peer.receive().await["method"], "fs/write_text_file");
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(observations.shutdowns.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn oversized_or_malformed_editor_text_never_becomes_a_tool_update() {
    for result in [
        json!({"content":"x".repeat(MAX_EDITOR_TEXT_BYTES + 1)}),
        json!({"content":123}),
    ] {
        let (mut peer, task, _, id) = open(true, true, ConnectionLimits::default()).await;
        peer.prompt(3, &id, "editor-read").await;
        let request = peer.receive().await;
        peer.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
            .await;
        let response = peer.receive().await;
        assert_eq!(response["id"], 3, "{response}");
        assert!(response["error"].is_object());
        drop(peer);
        assert!(task.await.unwrap().is_ok());
    }
}

#[tokio::test]
async fn two_sessions_cannot_receive_each_others_editor_replies() {
    let (mut peer, task, _, first) = open(true, true, ConnectionLimits::default()).await;
    peer.send(json!({
        "jsonrpc":"2.0","id":3,"method":"session/new",
        "params":{"cwd":"/workspace","mcpServers":[]}
    }))
    .await;
    let second = peer.receive().await["result"]["sessionId"].clone();
    assert!(second.is_string());
    peer.prompt(4, &first, "editor-read").await;
    let a = peer.receive().await;
    assert_eq!(a["method"], "fs/read_text_file");
    peer.prompt(5, &second, "editor-read").await;
    let b = peer.receive().await;
    assert_eq!(b["method"], "fs/read_text_file");
    assert_eq!(a["params"]["sessionId"], first);
    assert_eq!(b["params"]["sessionId"], second);
    for (request, text) in [(&b, "second-editor"), (&a, "first-editor")] {
        peer.send(json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":text}}))
            .await;
    }
    let mut observed = Vec::new();
    let mut finished = 0;
    while finished < 2 {
        let frame = peer.receive().await;
        if frame["id"].is_number() {
            assert_eq!(frame["result"]["stopReason"], "end_turn");
            finished += 1;
        } else if let Some(text) = frame["params"]["update"]["content"]["text"].as_str() {
            observed.push((frame["params"]["sessionId"].clone(), text.to_string()));
        }
    }
    assert!(observed.contains(&(first.clone(), "first-editor".into())));
    assert!(observed.contains(&(second.clone(), "second-editor".into())));
    assert!(!observed.contains(&(first, "second-editor".into())));
    drop(peer);
    assert!(task.await.unwrap().is_ok());
}
