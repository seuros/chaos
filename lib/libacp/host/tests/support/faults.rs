use super::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

pub(super) struct Peer {
    reader: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
}

impl Peer {
    pub(super) async fn prompt(&mut self, request: u64, session: &Value, text: &str) {
        self.send(
            json!({"jsonrpc":"2.0","id":request,"method":"session/prompt",
            "params":{"sessionId":session,"prompt":[{"type":"text","text":text}]}}),
        )
        .await;
    }

    pub(super) async fn send(&mut self, value: Value) {
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        self.writer.write_all(&line).await.unwrap();
    }

    pub(super) async fn receive(&mut self) -> Value {
        serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(2), self.reader.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
}

pub(super) fn raw(
    limits: ConnectionLimits,
) -> (Peer, JoinHandle<Result<(), HostError>>, Arc<Observations>) {
    let (a, task, observations) = serve(limits, 8192);
    let (reader, writer) = tokio::io::split(a);
    (
        Peer {
            reader: BufReader::new(reader).lines(),
            writer,
        },
        task,
        observations,
    )
}

pub(super) async fn open(
    limits: ConnectionLimits,
) -> (
    Peer,
    JoinHandle<Result<(), HostError>>,
    Arc<Observations>,
    Value,
) {
    let (mut peer, task, observations) = raw(limits);
    peer.send(json!({
        "jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}
    }))
    .await;
    assert!(peer.receive().await.get("error").is_none());
    peer.send(json!({
        "jsonrpc":"2.0","id":2,"method":"session/new",
        "params":{"cwd":"/workspace","mcpServers":[]}
    }))
    .await;
    let session = peer.receive().await["result"]["sessionId"].clone();
    assert!(session.is_string());
    (peer, task, observations, session)
}

#[tokio::test]
async fn initialization_is_required_and_cannot_be_repeated() {
    let (mut peer, task, observations) = raw(ConnectionLimits::default());
    peer.send(json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/workspace"}}))
        .await;
    assert!(peer.receive().await.get("error").is_some());
    for id in [2, 3] {
        peer.send(json!({
            "jsonrpc":"2.0","id":id,"method":"initialize","params":{"protocolVersion":1}
        }))
        .await;
        let reply = peer.receive().await;
        assert_eq!(reply.get("error").is_some(), id == 3);
    }
    drop(peer);
    let _ = task.await.unwrap();
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn malformed_and_oversized_frames_close_without_starting_native_work() {
    for bytes in [
        b"{broken\n".as_slice(),
        b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n".as_slice(),
    ] {
        let (mut peer, task, observations) = raw(ConnectionLimits {
            max_message_bytes: 32,
            ..Default::default()
        });
        peer.writer.write_all(bytes).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn local_invalid_limits_do_not_open_a_transport() {
    assert!(
        AgentServer::builder(Backend(Arc::new(Observations::default())))
            .limits(ConnectionLimits {
                max_sessions: 0,
                ..Default::default()
            })
            .build()
            .is_err()
    );
}

#[tokio::test]
async fn update_overflow_fails_closed_instead_of_dropping_output() {
    let (connection, _updates, task, observations) = start(ConnectionLimits {
        update_capacity: 1,
        ..Default::default()
    })
    .await;
    let id = session(&connection).await;
    assert!(
        connection
            .client()
            .request(prompt(&id, "flood"))
            .await
            .is_err()
    );
    assert!(task.await.unwrap().is_err());
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_serve_during_a_prompt_still_runs_bounded_native_cleanup() {
    let (connection, mut updates, task, observations) = start(ConnectionLimits::default()).await;
    let id = session(&connection).await;
    let client = connection.client();
    let prompt = tokio::spawn(async move { client.request(prompt(&id, "wait")).await });
    updates.recv().await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(prompt.await.unwrap().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while observations.shutdowns.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let _ = connection.disconnect().await;
}

#[tokio::test]
async fn unconfirmed_shutdown_is_an_error_not_success() {
    let (connection, _updates, task, observations) = start(ConnectionLimits::default()).await;
    session(&connection).await;
    observations.fail_shutdown.store(true, Ordering::SeqCst);
    connection.disconnect().await.unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Err(HostError::ShutdownUnconfirmed)
    ));
}

#[tokio::test]
async fn outgoing_frame_limits_apply_to_backend_updates() {
    let (connection, _updates, task, _) = start(ConnectionLimits {
        max_message_bytes: 1024,
        ..Default::default()
    })
    .await;
    let id = session(&connection).await;
    assert!(
        connection
            .client()
            .request(prompt(&id, "oversized"))
            .await
            .is_err()
    );
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn unsupported_content_is_rejected_without_invalidating_the_session() {
    let (mut peer, task, _, id) = open(ConnectionLimits::default()).await;
    peer.send(json!({
        "jsonrpc":"2.0","id":3,"method":"session/prompt",
        "params":{"sessionId":id,"prompt":[{"type":"image","data":"AA==","mimeType":"image/png"}]}
    }))
    .await;
    assert!(peer.receive().await.get("error").is_some());
    peer.send(json!({
        "jsonrpc":"2.0","id":4,"method":"session/prompt",
        "params":{"sessionId":id,"prompt":[
            {"type":"resource_link","name":"file","uri":"file:///workspace/file"}
        ]}
    }))
    .await;
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "1:resource"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    drop(peer);
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn old_cancel_hook_cannot_outlive_the_turn_and_interrupt_a_new_prompt() {
    let (mut peer, task, observations, id) = open(ConnectionLimits::default()).await;
    observations.slow_cancel.store(true, Ordering::SeqCst);
    peer.send(json!({
        "jsonrpc":"2.0","id":3,"method":"session/prompt",
        "params":{"sessionId":id,"prompt":[{"type":"text","text":"wait"}]}
    }))
    .await;
    peer.receive().await;
    peer.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
        .await;
    peer.send(json!({
        "jsonrpc":"2.0","id":4,"method":"session/prompt",
        "params":{"sessionId":id,"prompt":[{"type":"text","text":"overlap"}]}
    }))
    .await;
    let busy = peer.receive().await;
    assert_eq!(busy["id"], 4);
    assert!(busy.get("error").is_some());
    observations.cancel_release.notify_one();
    let cancelled = peer.receive().await;
    assert_eq!(cancelled["id"], 3);
    assert_eq!(cancelled["result"]["stopReason"], "cancelled");
    peer.send(json!({
        "jsonrpc":"2.0","id":5,"method":"session/prompt",
        "params":{"sessionId":id,"prompt":[{"type":"text","text":"after"}]}
    }))
    .await;
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "2:after"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    assert_eq!(observations.cancels.load(Ordering::SeqCst), 1);
    drop(peer);
    let _ = task.await.unwrap();
}
