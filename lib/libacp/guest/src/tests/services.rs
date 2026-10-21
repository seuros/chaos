use super::*;
use protocol::schema::v1::*;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct Editor {
    calls: Mutex<Vec<Value>>,
}

impl FileReader for Editor {
    fn read_text_file(
        &self,
        request: ReadTextFileRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<ReadTextFileResponse>> {
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        Box::pin(async { Ok(ReadTextFileResponse::new("unsaved editor text")) })
    }
}
impl FileWriter for Editor {
    fn write_text_file(
        &self,
        request: WriteTextFileRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<WriteTextFileResponse>> {
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        Box::pin(async { Ok(WriteTextFileResponse::new()) })
    }
}

#[derive(Debug, Default)]
struct Terminals {
    killed: tokio::sync::Notify,
    started: tokio::sync::Notify,
}
impl TerminalHandler for Terminals {
    fn create(
        &self,
        request: CreateTerminalRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<CreateTerminalResponse>> {
        assert_eq!(request.session_id, SessionId::new("s"));
        assert_eq!(request.command, "approved-command");
        assert_eq!(request.args, ["--fixture"]);
        assert_eq!(request.output_byte_limit, Some(12));
        Box::pin(async { Ok(CreateTerminalResponse::new("owned-by-s")) })
    }
    fn output(
        &self,
        request: TerminalOutputRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<TerminalOutputResponse>> {
        Box::pin(async move {
            if request.session_id != SessionId::new("s")
                || request.terminal_id != TerminalId::new("owned-by-s")
            {
                return Err(protocol::Error::invalid_params());
            }
            Ok(TerminalOutputResponse::new("utf8: é", true))
        })
    }
    fn release(
        &self,
        _: ReleaseTerminalRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<ReleaseTerminalResponse>> {
        Box::pin(async { Ok(ReleaseTerminalResponse::new()) })
    }
    fn wait_for_exit(
        &self,
        _: WaitForTerminalExitRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<WaitForTerminalExitResponse>> {
        Box::pin(async move {
            self.started.notify_one();
            self.killed.notified().await;
            Ok(WaitForTerminalExitResponse::new(
                TerminalExitStatus::new().signal("TERM".to_owned()),
            ))
        })
    }
    fn kill(
        &self,
        _: KillTerminalRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<KillTerminalResponse>> {
        Box::pin(async move {
            self.killed.notify_one();
            Ok(KillTerminalResponse::new())
        })
    }
}

async fn call(peer: &mut Peer, id: u64, method: &str, params: Value) -> Value {
    peer.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .await;
    let reply = peer.receive().await.unwrap();
    assert_eq!(reply["id"], id);
    reply
}

#[tokio::test]
async fn editor_capabilities_are_independent_paths_are_absolute_and_fields_preserved() {
    let editor = Arc::new(Editor::default());
    let (connection, _, mut peer) = connect(ConnectionOptions {
        file_reader: Some(editor.clone()),
        ..Default::default()
    })
    .await;
    let result = call(
        &mut peer,
        1,
        "fs/read_text_file",
        json!({"sessionId":"s","path":"/workspace/buffer","line":2,"limit":4}),
    )
    .await;
    assert_eq!(result["result"]["content"], "unsaved editor text");
    assert_eq!(editor.calls.lock().unwrap()[0]["line"], 2);
    assert_eq!(editor.calls.lock().unwrap()[0]["limit"], 4);
    for (method, params) in [
        (
            "fs/read_text_file",
            json!({"sessionId":"s","path":"relative"}),
        ),
        (
            "fs/write_text_file",
            json!({"sessionId":"s","path":"/workspace/buffer","content":"denied"}),
        ),
        (
            "terminal/create",
            json!({"sessionId":"s","command":"not-run"}),
        ),
    ] {
        assert!(call(&mut peer, 2, method, params).await["error"].is_object());
    }
    assert_eq!(editor.calls.lock().unwrap().len(), 1);
    connection.disconnect().await.unwrap();
    let (connection, _, mut peer) = connect(ConnectionOptions {
        file_writer: Some(editor.clone()),
        ..Default::default()
    })
    .await;
    assert!(
        call(
            &mut peer,
            3,
            "fs/write_text_file",
            json!({"sessionId":"s","path":"/workspace/buffer","content":"replacement"})
        )
        .await["result"]
            .is_object()
    );
    assert_eq!(editor.calls.lock().unwrap()[1]["content"], "replacement");
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn all_terminal_methods_dispatch_and_pending_wait_does_not_block_kill() {
    let terminals = Arc::new(Terminals::default());
    let (connection, _, mut peer) = connect(ConnectionOptions {
        terminals: Some(terminals.clone()),
        ..Default::default()
    })
    .await;
    assert!(
        call(
            &mut peer,
            1,
            "terminal/create",
            json!({"sessionId":"s","command":"x","cwd":"relative"})
        )
        .await["error"]
            .is_object()
    );
    let created = call(&mut peer, 2, "terminal/create", json!({"sessionId":"s","command":"approved-command","args":["--fixture"],"cwd":"/workspace","outputByteLimit":12})).await;
    assert_eq!(created["result"]["terminalId"], "owned-by-s");
    let output = call(
        &mut peer,
        3,
        "terminal/output",
        json!({"sessionId":"s","terminalId":"owned-by-s"}),
    )
    .await;
    assert_eq!(output["result"]["output"], "utf8: é");
    assert_eq!(output["result"]["truncated"], true);
    assert!(
        call(
            &mut peer,
            4,
            "terminal/output",
            json!({"sessionId":"foreign","terminalId":"owned-by-s"})
        )
        .await["error"]
            .is_object()
    );
    peer.send(json!({"jsonrpc":"2.0","id":5,"method":"terminal/wait_for_exit","params":{"sessionId":"s","terminalId":"owned-by-s"}})).await;
    terminals.started.notified().await;
    peer.send(json!({"jsonrpc":"2.0","id":6,"method":"terminal/kill","params":{"sessionId":"s","terminalId":"owned-by-s"}})).await;
    let mut replies = [peer.receive().await.unwrap(), peer.receive().await.unwrap()];
    replies.sort_by_key(|reply| reply["id"].as_u64());
    assert_eq!(replies[0]["result"]["signal"], "TERM");
    assert!(replies[1]["result"].is_object());
    assert!(
        call(
            &mut peer,
            7,
            "terminal/release",
            json!({"sessionId":"s","terminalId":"owned-by-s"})
        )
        .await["result"]
            .is_object()
    );
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn service_admission_and_deadlines_are_bounded_without_retries() {
    let terminals = Arc::new(Terminals::default());
    let (connection, _, mut peer) = connect(ConnectionOptions {
        terminals: Some(terminals.clone()),
        pending_services: 1,
        request_timeout: Duration::from_millis(100),
        ..Default::default()
    })
    .await;
    peer.send(json!({"jsonrpc":"2.0","id":1,"method":"terminal/wait_for_exit","params":{"sessionId":"s","terminalId":"owned-by-s"}})).await;
    terminals.started.notified().await;
    assert!(
        call(
            &mut peer,
            2,
            "terminal/output",
            json!({"sessionId":"s","terminalId":"owned-by-s"})
        )
        .await["error"]
            .is_object()
    );
    let expired = peer.receive().await.unwrap();
    assert_eq!(expired["id"], 1);
    assert!(expired["error"].is_object());
    assert!(
        call(
            &mut peer,
            3,
            "terminal/output",
            json!({"sessionId":"s","terminalId":"owned-by-s"})
        )
        .await["result"]
            .is_object()
    );
    connection.disconnect().await.unwrap();
}
