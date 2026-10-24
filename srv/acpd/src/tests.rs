use chaos_snitch::tracing::subscriber as tracing_subscriber;
use std::time::Duration;

use acp_guest::{AcpConnection, ConnectionOptions};
use acp_host::protocol::schema::v1::{
    ContentBlock, NewSessionRequest, PromptRequest, StopReason, TextContent,
};
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_kern::config::Config;
use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_response_created, mount_sse_sequence, sse,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use wiremock::MockServer;

mod active_controls;
#[cfg(unix)]
mod cancellation;
mod commands;
mod controls;
mod creation;
mod elicitation;
mod file_faults;
mod files;
mod history;
mod journal;
mod lifecycle;
mod permissions;
mod validation;

struct Fixture {
    _home: TempDir,
    cwd: TempDir,
    server: MockServer,
    config: Config,
}

impl Fixture {
    async fn new() -> Self {
        journal::start();
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let server = MockServer::start().await;
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let mut config = core_test_support::load_default_config_for_test(&home).await;
        config.cwd = cwd.path().to_path_buf();
        config.model = Some("mock-model".into());
        config.model_catalog = Some(chaos_kern::test_support::test_models_response(&[
            "mock-model",
        ]));
        config.model_provider_id = "acp-fixture".into();
        config.model_provider = serde_json::from_value(json!({
            "name":"Isolated ACP fixture", "base_url":format!("{}/v1", server.uri()),
            "wire_api":"responses", "request_max_retries":0,"stream_max_retries":0,
            "stream_idle_timeout_ms":5000
        }))
        .unwrap();
        // Catalog discovery must never probe ambient accounts from the developer's environment.
        config.model_providers = std::collections::HashMap::from([(
            config.model_provider_id.clone(),
            config.model_provider.clone(),
        )]);
        Self {
            _home: home,
            cwd,
            server,
            config,
        }
    }

    fn supervised(&mut self) {
        self.config
            .permissions
            .approval_policy
            .set(ApprovalPolicy::Supervised)
            .unwrap();
    }

    fn patch(&self, name: &str, replacement: &str) -> (std::path::PathBuf, String) {
        let file = self.cwd.path().join(name);
        std::fs::write(&file, "before\n").unwrap();
        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n-before\n+{replacement}\n*** End Patch",
            file.display(),
        );
        (file, patch)
    }

    async fn peer(&self) -> (Peer, Value) {
        Peer::open(self.config.clone()).await
    }
}

fn native_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("native ACP fixture runtime")
}

async fn serve(config: Config) -> (DuplexStream, JoinHandle<Result<(), acp_host::HostError>>) {
    let host = super::build_server(config).await.unwrap();
    let (a, b) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(b);
    let task = tokio::task::spawn_blocking(move || {
        native_runtime().block_on(Box::pin(host.serve(reader, writer)))
    });
    (a, task)
}

fn response(id: &str, text: &str) -> String {
    sse(vec![
        ev_response_created(id),
        ev_assistant_message(id, text),
        ev_completed(id),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_continued_turns_and_two_native_sessions() {
    let fixture = Fixture::new().await;
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![
            sse(vec![
                ev_response_created("r1"),
                core_test_support::responses::ev_reasoning_item_added("private", &[]),
                core_test_support::responses::ev_reasoning_summary_text_delta("private reasoning"),
                core_test_support::responses::ev_message_item_added("msg1", ""),
                core_test_support::responses::ev_output_text_delta("first native "),
                core_test_support::responses::ev_output_text_delta("response"),
                ev_assistant_message("msg1", "first native response"),
                ev_completed("r1"),
            ]),
            response("r2", "continued native response"),
            response("r3", "isolated native response"),
        ],
    )
    .await;
    let (a, task) = serve(fixture.config.clone()).await;
    let (reader, writer) = tokio::io::split(a);
    let (connection, mut updates) = AcpConnection::connect(
        reader,
        writer,
        ConnectionOptions {
            request_timeout: Duration::from_secs(20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let first = connection
        .client()
        .request(NewSessionRequest::new(fixture.cwd.path().to_owned()))
        .await
        .unwrap()
        .session_id;
    let second = connection
        .client()
        .request(NewSessionRequest::new(fixture.cwd.path().to_owned()))
        .await
        .unwrap()
        .session_id;
    assert_ne!(first, second);
    for (id, input, expected) in [
        (&first, "first private input", "first native response"),
        (
            &first,
            "continued private input",
            "continued native response",
        ),
        (
            &second,
            "separate private input",
            "isolated native response",
        ),
    ] {
        let result = connection
            .client()
            .request(PromptRequest::new(
                id.clone(),
                vec![ContentBlock::Text(TextContent::new(input))],
            ))
            .await
            .unwrap();
        assert_eq!(result.stop_reason, StopReason::EndTurn);
        let mut text = String::new();
        while let Ok(update) = updates.try_recv() {
            if matches!(
                update.update,
                acp_host::protocol::schema::v1::SessionUpdate::AvailableCommandsUpdate(_)
            ) {
                assert!(update.session_id == first || update.session_id == second);
                continue;
            }
            assert_eq!(update.session_id, *id);
            let value = serde_json::to_value(update).unwrap();
            if value["update"]["sessionUpdate"] == "agent_message_chunk" {
                text.push_str(value["update"]["content"]["text"].as_str().unwrap());
            }
            assert!(!value.to_string().contains("agent_thought_chunk"));
            assert!(!value.to_string().contains("private reasoning"));
        }
        assert_eq!(text, expected);
    }
    let bodies: Vec<_> = requests
        .requests()
        .iter()
        .map(core_test_support::responses::ResponsesRequest::body_json)
        .collect();
    assert!(bodies[1].to_string().contains("first private input"));
    assert!(!bodies[2].to_string().contains("first private input"));
    connection.disconnect().await.unwrap();
    join_server(task).await;
}

async fn join_server(task: JoinHandle<Result<(), acp_host::HostError>>) {
    tokio::time::timeout(Duration::from_secs(6), task)
        .await
        .expect("native ACP server shutdown deadline")
        .expect("native ACP server task")
        .expect("native ACP server shutdown");
}

struct Peer {
    initial: Value,
    initialized: Value,
    reader: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
    task: JoinHandle<Result<(), acp_host::HostError>>,
    next: u64,
}

impl Peer {
    async fn open(config: Config) -> (Self, Value) {
        let cwd = config.cwd.clone();
        let mut peer = Self::connect(config).await;
        let (result, _) = peer
            .request("session/new", json!({"cwd":cwd,"mcpServers":[]}))
            .await;
        peer.initial = result.clone();
        let id = result["result"]["sessionId"].clone();
        assert!(id.is_string(), "{result}");
        (peer, id)
    }

    async fn connect(config: Config) -> Self {
        Self::connect_capabilities(config, json!({})).await
    }

    async fn connect_capabilities(config: Config, capabilities: Value) -> Self {
        let (a, task) = serve(config).await;
        let (reader, writer) = tokio::io::split(a);
        let mut peer = Self {
            initial: Value::Null,
            initialized: Value::Null,
            reader: BufReader::new(reader).lines(),
            writer,
            task,
            next: 2,
        };
        peer.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":1,"clientCapabilities":capabilities}}))
            .await;
        peer.initialized = peer.receive().await;
        assert!(peer.initialized.get("error").is_none());
        peer
    }

    async fn send(&mut self, value: Value) {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.unwrap();
    }

    async fn receive(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(20), self.reader.next_line())
            .await
            .expect("native ACP frame deadline")
            .unwrap()
            .expect("ACP EOF");
        serde_json::from_str(&line).unwrap()
    }

    async fn prompt(&mut self, session: &Value, text: &str) -> u64 {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
            "params":{"sessionId":session,"prompt":[{"type":"text","text":text}]}}))
            .await;
        id
    }

    async fn request(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        let mut updates = Vec::new();
        loop {
            let frame = self.receive().await;
            if frame["id"] == id {
                return (frame, updates);
            }
            assert_eq!(frame["method"], "session/update", "{frame}");
            updates.push(frame);
        }
    }

    async fn cancel(&mut self, session: &Value) {
        self.send(json!({"jsonrpc":"2.0","method":"session/cancel",
            "params":{"sessionId":session}}))
            .await;
    }

    async fn permission(&mut self, session: &Value) -> Value {
        loop {
            let frame = self.receive().await;
            assert_eq!(&frame["params"]["sessionId"], session, "{frame}");
            if frame["method"] == "session/request_permission" {
                return frame;
            }
            assert_eq!(frame["method"], "session/update", "{frame}");
        }
    }

    async fn reply_permission(&mut self, request: &Value, option: &str) {
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":{
            "outcome":{"outcome":"selected","optionId":option}}}))
            .await;
    }

    async fn finish(&mut self, session: &Value, request: u64, reason: &str) -> Vec<Value> {
        let mut updates = Vec::new();
        loop {
            let frame = self.receive().await;
            if frame["id"] == request {
                assert_eq!(frame["result"]["stopReason"], reason, "{frame}");
                return updates;
            }
            assert_eq!(frame["method"], "session/update", "{frame}");
            assert_eq!(&frame["params"]["sessionId"], session, "{frame}");
            updates.push(frame);
        }
    }

    async fn close(self) {
        drop(self.reader);
        drop(self.writer);
        join_server(self.task).await;
    }
}
