#![cfg(test)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use acp_host::protocol::schema::v1::*;
use acp_host::{
    AgentBackend, AgentServer, AgentSession, HostError, SessionContext, SessionInfo, protocol,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;

#[path = "conformance/validation.rs"]
mod validation;

#[derive(Clone, Default)]
struct Backend {
    opens: Arc<AtomicUsize>,
}

impl AgentBackend for Backend {
    fn session_capabilities(&self) -> SessionCapabilities {
        SessionCapabilities::new()
            .list(SessionListCapabilities::new())
            .resume(SessionResumeCapabilities::new())
            .close(SessionCloseCapabilities::new())
            .delete(SessionDeleteCapabilities::new())
    }

    fn load_session_supported(&self) -> bool {
        true
    }

    fn new_session(
        &self,
        _: NewSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Arc::new(Session) as Arc<dyn AgentSession>) })
    }

    fn load_session(
        &self,
        _: LoadSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        self.new_session(NewSessionRequest::new("/workspace"))
    }

    fn resume_session(
        &self,
        _: ResumeSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        self.new_session(NewSessionRequest::new("/workspace"))
    }
}

struct Session;

fn info() -> SessionInfo {
    SessionInfo {
        id: Some(SessionId::new("s")),
        config_options: Some(
            serde_json::from_value(json!([
                {"id":"flag","name":"Flag","type":"boolean","currentValue":true},
                {"id":"mode","name":"Mode","type":"select","currentValue":"ask",
                 "options":[{"value":"ask","name":"Ask"},{"value":"code","name":"Code"}]}
            ]))
            .unwrap(),
        ),
        modes: Some(SessionModeState::new(
            "ask",
            vec![
                SessionMode::new("ask", "Ask"),
                SessionMode::new("code", "Code"),
            ],
        )),
        ..Default::default()
    }
}

impl AgentSession for Session {
    fn info(&self) -> protocol::BoxFuture<'_, protocol::Result<SessionInfo>> {
        Box::pin(async { Ok(info()) })
    }

    fn set_config_option(
        &self,
        request: SetSessionConfigOptionRequest,
        _: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<SessionInfo>> {
        Box::pin(async move {
            let mut info = info();
            for option in info.config_options.as_mut().unwrap() {
                if option.id != request.config_id {
                    continue;
                }
                match (&mut option.kind, &request.value) {
                    (
                        SessionConfigKind::Boolean(option),
                        SessionConfigOptionValue::Boolean { value },
                    ) => {
                        option.current_value = *value;
                    }
                    (
                        SessionConfigKind::Select(option),
                        SessionConfigOptionValue::ValueId { value },
                    ) => {
                        option.current_value = value.clone();
                        info.modes.as_mut().unwrap().current_mode_id =
                            SessionModeId::new(value.0.clone());
                    }
                    _ => return Err(protocol::Error::invalid_params()),
                }
            }
            Ok(info)
        })
    }

    fn replay(&self, context: SessionContext) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async move {
            for update in optional_updates() {
                context.client().update_wait(update).await?;
            }
            Ok(())
        })
    }

    fn prompt(
        &self,
        request: PromptRequest,
        context: SessionContext,
    ) -> protocol::BoxFuture<'_, protocol::Result<PromptResponse>> {
        Box::pin(async move {
            for update in optional_updates() {
                context.client().update(update)?;
            }
            if !request.prompt.is_empty() {
                context.cancelled().await;
                return Ok(PromptResponse::new(StopReason::Cancelled));
            }
            Ok(PromptResponse::new(StopReason::EndTurn))
        })
    }

    fn cancel(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn shutdown(&self) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

fn optional_updates() -> Vec<SessionUpdate> {
    vec![
        SessionUpdate::Notice(Notice::new(NoticeSeverity::Info, "Notice")),
        SessionUpdate::CompactionUpdate(CompactionUpdate::new("c", CompactionStatus::Completed)),
        SessionUpdate::CompactionSummaryChunk(CompactionSummaryChunk::new(
            "c",
            ContentBlock::Text(TextContent::new("summary")),
        )),
        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(info().config_options.unwrap())),
    ]
}

struct Peer {
    reader: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
    task: JoinHandle<Result<(), HostError>>,
    backend: Backend,
}

impl Peer {
    async fn new(capabilities: Value) -> (Self, Value) {
        let backend = Backend::default();
        let server = AgentServer::builder(backend.clone()).build().unwrap();
        let (client, stream) = tokio::io::duplex(65536);
        let (reader, writer) = tokio::io::split(stream);
        let task = tokio::spawn(async move { server.serve(reader, writer).await });
        let (reader, writer) = tokio::io::split(client);
        let mut peer = Self {
            reader: BufReader::new(reader).lines(),
            writer,
            task,
            backend,
        };
        peer.request(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":capabilities}),
        )
        .await;
        let initialized = peer.receive().await;
        (peer, initialized)
    }

    async fn send(&mut self, frame: Value) {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.unwrap();
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
    }

    async fn receive(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(2), self.reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn finish(self) {
        drop(self.writer);
        drop(self.reader);
        assert!(self.task.await.unwrap().is_ok());
    }
}

#[tokio::test]
async fn unimplemented_capabilities_are_never_advertised() {
    let (peer, initialized) = Peer::new(json!({})).await;
    let capabilities = &initialized["result"]["agentCapabilities"]["sessionCapabilities"];
    assert!(capabilities.get("close").is_none());
    assert!(capabilities.get("delete").is_none());
    assert_eq!(capabilities["list"], json!({}));
    assert_eq!(capabilities["resume"], json!({}));
    peer.finish().await;
}

#[tokio::test]
async fn lifecycle_responses_and_config_updates_filter_unnegotiated_booleans() {
    for method in ["session/new", "session/load", "session/resume"] {
        let (mut peer, _) = Peer::new(json!({})).await;
        peer.request(
            2,
            method,
            json!({"cwd":"/workspace","sessionId":"s","mcpServers":[]}),
        )
        .await;
        // Loading replays only the permitted config update before responding.
        if method == "session/load" {
            let replay = peer.receive().await;
            assert_eq!(
                replay["params"]["update"]["sessionUpdate"],
                "config_option_update"
            );
            assert_eq!(
                replay["params"]["update"]["configOptions"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
        let response = peer.receive().await;
        assert_eq!(
            response["result"]["configOptions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(response["result"]["configOptions"][0]["id"], "mode");
        peer.request(
            3,
            "session/set_config_option",
            json!({"sessionId":"s","configId":"mode","value":"ask"}),
        )
        .await;
        let update = peer.receive().await;
        assert_eq!(
            update["params"]["update"]["configOptions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        peer.receive().await; // current_mode_update
        assert_eq!(
            peer.receive().await["result"]["configOptions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        peer.request(
            4,
            "session/set_config_option",
            json!({"sessionId":"s","configId":"flag","type":"boolean","value":false}),
        )
        .await;
        assert_eq!(peer.receive().await["error"]["code"], -32602);
        peer.finish().await;
    }
}

#[tokio::test]
async fn optional_updates_require_explicit_nonnull_capabilities_live_and_in_replay() {
    for capabilities in [
        json!({}),
        json!({"session":null}),
        json!({"session":{"notices":null,"compaction":null,"configOptions":{"boolean":null}}}),
        json!({"session":{"notices":{},"compaction":{},"configOptions":{"boolean":{}}}}),
    ] {
        let supported = capabilities["session"]["notices"].is_object();
        for method in ["session/new", "session/load"] {
            let (mut peer, _) = Peer::new(capabilities.clone()).await;
            peer.request(
                2,
                method,
                json!({"sessionId":"s","cwd":"/workspace","mcpServers":[]}),
            )
            .await;
            if method == "session/load" {
                receive_optional_updates(&mut peer, supported).await;
            }
            let opened = peer.receive().await;
            assert_eq!(
                opened["result"]["configOptions"].as_array().unwrap().len(),
                if supported { 2 } else { 1 }
            );
            peer.request(3, "session/prompt", json!({"sessionId":"s","prompt":[]}))
                .await;
            receive_optional_updates(&mut peer, supported).await;
            assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
            peer.finish().await;
        }
    }
}

async fn receive_optional_updates(peer: &mut Peer, supported: bool) {
    if supported {
        for expected in ["notice", "compaction_update", "compaction_summary_chunk"] {
            assert_eq!(
                peer.receive().await["params"]["update"]["sessionUpdate"],
                expected
            );
        }
    }
    let config = peer.receive().await;
    assert_eq!(
        config["params"]["update"]["sessionUpdate"],
        "config_option_update"
    );
    assert_eq!(
        config["params"]["update"]["configOptions"]
            .as_array()
            .unwrap()
            .len(),
        if supported { 2 } else { 1 }
    );
}

#[tokio::test]
async fn supported_controls_are_available_during_prompts_without_releasing_the_prompt() {
    let (mut peer, _) = Peer::new(json!({})).await;
    peer.request(
        2,
        "session/new",
        json!({"cwd":"/workspace","mcpServers":[]}),
    )
    .await;
    peer.receive().await;
    peer.request(
        3,
        "session/prompt",
        json!({"sessionId":"s","prompt":[{"type":"text","text":"wait"}]}),
    )
    .await;
    receive_optional_updates(&mut peer, false).await;
    for (id, method, params) in [
        (
            4,
            "session/set_config_option",
            json!({"sessionId":"s","configId":"mode","value":"code"}),
        ),
        (
            5,
            "session/set_mode",
            json!({"sessionId":"s","modeId":"code"}),
        ),
    ] {
        peer.request(id, method, params).await;
        let config = peer.receive().await;
        assert_eq!(
            config["params"]["update"]["configOptions"][0]["currentValue"],
            "code"
        );
        let mode = peer.receive().await;
        assert_eq!(mode["params"]["update"]["currentModeId"], "code");
        let response = peer.receive().await;
        assert_eq!(response["id"], id);
        assert!(response.get("result").is_some());
    }
    peer.request(6, "session/prompt", json!({"sessionId":"s","prompt":[]}))
        .await;
    assert_eq!(peer.receive().await["error"]["code"], -32602);
    peer.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s"}}))
        .await;
    assert_eq!(peer.receive().await["result"]["stopReason"], "cancelled");
    peer.finish().await;
}
