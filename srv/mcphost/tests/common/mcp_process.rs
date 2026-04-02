use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;

use anyhow::Context;
use chaos_mcphost::ChaosToolParams;

use mcp_host::protocol::capabilities::ClientCapabilities;
use mcp_host::protocol::capabilities::ElicitationCapability;
use mcp_host::protocol::capabilities::FormElicitationCapability;
use mcp_host::protocol::capabilities::InitializeRequest;
use mcp_host::protocol::types::CallToolRequestParams;
use mcp_host::protocol::types::Implementation;
use mcp_host::protocol::types::JsonRpcMessage;
use mcp_host::protocol::types::JsonRpcRequest;
use mcp_host::protocol::types::JsonRpcResponse;
use mcp_host::protocol::types::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::process::Command;

pub struct McpProcess {
    next_request_id: AtomicI64,
    /// Retain this child process until the client is dropped. The Tokio runtime
    /// will make a "best effort" to reap the process after it exits, but it is
    /// not a guarantee. See the `kill_on_drop` documentation for details.
    #[allow(dead_code)]
    process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Messages read from the stream that were skipped by one reader but may be
    /// needed by the next (e.g. a Response that arrived while waiting for a
    /// notification). Checked first by every read helper.
    pending: Vec<JsonRpcMessage>,
}

impl McpProcess {
    pub async fn new(chaos_home: &Path) -> anyhow::Result<Self> {
        Self::new_with_env(chaos_home, &[]).await
    }

    /// Creates a new MCP process, allowing tests to override or remove
    /// specific environment variables for the child process only.
    ///
    /// Pass a tuple of (key, Some(value)) to set/override, or (key, None) to
    /// remove a variable from the child's environment.
    pub async fn new_with_env(
        chaos_home: &Path,
        env_overrides: &[(&str, Option<&str>)],
    ) -> anyhow::Result<Self> {
        let program = chaos_which::cargo_bin("chaos").context("should find binary for chaos")?;
        let mut cmd = Command::new(program);
        cmd.arg("mcp").arg("serve");

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.env("CHAOS_HOME", chaos_home);
        cmd.env("RUST_LOG", "debug");

        for (k, v) in env_overrides {
            match v {
                Some(val) => {
                    cmd.env(k, val);
                }
                None => {
                    cmd.env_remove(k);
                }
            }
        }

        let mut process = cmd
            .kill_on_drop(true)
            .spawn()
            .context("chaos-mcphost proc should start")?;
        let stdin = process
            .stdin
            .take()
            .ok_or_else(|| anyhow::format_err!("mcp should have stdin fd"))?;
        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| anyhow::format_err!("mcp should have stdout fd"))?;
        let stdout = BufReader::new(stdout);

        // Forward child's stderr to our stderr so failures are visible even
        // when stdout/stderr are captured by the test harness.
        if let Some(stderr) = process.stderr.take() {
            let mut stderr_reader = BufReader::new(stderr).lines();
            tokio::spawn(async move {
                while let Ok(Some(line)) = stderr_reader.next_line().await {
                    eprintln!("[mcp stderr] {line}");
                }
            });
        }
        Ok(Self {
            next_request_id: AtomicI64::new(0),
            process,
            stdin,
            stdout,
            pending: Vec::new(),
        })
    }

    /// Performs the initialization handshake with the MCP server.
    pub async fn initialize(&mut self) -> anyhow::Result<()> {
        let initialized = self
            .initialize_with_protocol_version_and_capabilities(
                "2025-11-25",
                ClientCapabilities {
                    elicitation: Some(ElicitationCapability {
                        form: Some(FormElicitationCapability {}),
                        url: None,
                    }),
                    experimental: None,
                    roots: None,
                    sampling: None,
                    tasks: None,
                },
            )
            .await?;
        let JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc,
            id,
            result,
            error: _,
        }) = initialized
        else {
            anyhow::bail!("expected initialize response message, got: {initialized:?}")
        };
        assert_eq!(jsonrpc, "2.0");
        assert_eq!(id, json!(0));
        assert_eq!(
            result.as_ref().unwrap(),
            &json!({
                "capabilities": {
                    "tools": {
                        "listChanged": true
                    },
                    "resources": {
                        "listChanged": true,
                        "subscribe": false,
                        "listTemplates": true
                    },
                },
                "instructions": "Chaos — provider-agnostic coding agent",
                "serverInfo": {
                    "name": "chaos-mcp-server",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "protocolVersion": "2025-11-25"
            })
        );

        self.send_initialized_notification().await?;

        Ok(())
    }

    pub async fn initialize_with_protocol_version(
        &mut self,
        protocol_version: &str,
    ) -> anyhow::Result<JsonRpcMessage> {
        self.initialize_with_protocol_version_and_capabilities(
            protocol_version,
            ClientCapabilities {
                elicitation: Some(ElicitationCapability {
                    form: Some(FormElicitationCapability {}),
                    url: None,
                }),
                experimental: None,
                roots: None,
                sampling: None,
                tasks: None,
            },
        )
        .await
    }

    pub async fn initialize_without_elicitation(&mut self) -> anyhow::Result<()> {
        let initialized = self
            .initialize_with_protocol_version_and_capabilities(
                "2025-11-25",
                ClientCapabilities {
                    elicitation: None,
                    experimental: None,
                    roots: None,
                    sampling: None,
                    tasks: None,
                },
            )
            .await?;
        let JsonRpcMessage::Response(_) = initialized else {
            anyhow::bail!("expected initialize response message, got: {initialized:?}")
        };
        self.send_initialized_notification().await?;
        Ok(())
    }

    pub async fn initialize_with_protocol_version_and_capabilities(
        &mut self,
        protocol_version: &str,
        capabilities: ClientCapabilities,
    ) -> anyhow::Result<JsonRpcMessage> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let params = InitializeRequest {
            capabilities,
            client_info: Implementation {
                name: "elicitation test".into(),
                title: Some("Elicitation Test".into()),
                version: "0.0.0".into(),
                description: None,
                icons: None,
                website_url: None,
            },
            protocol_version: protocol_version.to_string(),
        };
        let params_value = serde_json::to_value(params)?;

        self.send_jsonrpc_message(JsonRpcMessage::Request(JsonRpcRequest::new(
            json!(request_id),
            "initialize",
            Some(params_value),
        )))
        .await?;

        self.read_jsonrpc_message().await
    }

    pub async fn send_initialized_notification(&mut self) -> anyhow::Result<()> {
        self.send_jsonrpc_message(JsonRpcMessage::Notification(JsonRpcRequest::notification(
            "notifications/initialized",
            None,
        )))
        .await
    }

    /// Returns the id used to make the request so it can be used when
    /// correlating notifications.
    pub async fn send_chaos_tool_call(&mut self, params: ChaosToolParams) -> anyhow::Result<i64> {
        let codex_tool_call_params = CallToolRequestParams {
            meta: None,
            name: "chaos".into(),
            arguments: Some(match serde_json::to_value(params)? {
                serde_json::Value::Object(map) => map,
                _ => unreachable!("params serialize to object"),
            }),
        };
        self.send_request(
            "tools/call",
            Some(serde_json::to_value(codex_tool_call_params)?),
        )
        .await
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<i64> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);

        let message =
            JsonRpcMessage::Request(JsonRpcRequest::new(json!(request_id), method, params));
        self.send_jsonrpc_message(message).await?;
        Ok(request_id)
    }

    pub async fn send_custom_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<RequestId> {
        Ok(RequestId::Number(self.send_request(method, params).await?))
    }

    pub async fn send_response(
        &mut self,
        id: RequestId,
        result: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.send_jsonrpc_message(JsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: id.to_value(),
            result: Some(result),
            error: None,
        }))
        .await
    }

    async fn send_jsonrpc_message(&mut self, message: JsonRpcMessage) -> anyhow::Result<()> {
        eprintln!("writing message to stdin: {message:?}");
        let payload = serde_json::to_string(&message)?;
        self.stdin.write_all(payload.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_jsonrpc_message(&mut self) -> anyhow::Result<JsonRpcMessage> {
        // Drain pending buffer before hitting the stream.
        if !self.pending.is_empty() {
            let message = self.pending.remove(0);
            eprintln!("read message from pending: {message:?}");
            return Ok(message);
        }
        let mut line = String::new();
        self.stdout.read_line(&mut line).await?;
        let message = serde_json::from_str::<JsonRpcMessage>(&line)?;
        eprintln!("read message from stdout: {message:?}");
        Ok(message)
    }

    pub async fn read_next_jsonrpc_message(&mut self) -> anyhow::Result<JsonRpcMessage> {
        self.read_jsonrpc_message().await
    }

    pub async fn read_stream_until_request_message(&mut self) -> anyhow::Result<JsonRpcRequest> {
        eprintln!("in read_stream_until_request_message()");

        loop {
            let message = self.read_jsonrpc_message().await?;

            match message {
                JsonRpcMessage::Notification(_) => {
                    eprintln!("notification: {message:?}");
                }
                JsonRpcMessage::Request(jsonrpc_request) => {
                    return Ok(jsonrpc_request);
                }
                JsonRpcMessage::Response(resp) if resp.error.is_some() => {
                    anyhow::bail!("unexpected JSONRPCMessage error response: {resp:?}");
                }
                JsonRpcMessage::Response(_) => {
                    anyhow::bail!("unexpected JSONRPCMessage::Response: {message:?}");
                }
            }
        }
    }

    pub async fn read_stream_until_response_message(
        &mut self,
        request_id: RequestId,
    ) -> anyhow::Result<JsonRpcResponse> {
        eprintln!("in read_stream_until_response_message({request_id:?})");
        let id_value = request_id.to_value();

        loop {
            let message = self.read_jsonrpc_message().await?;
            match message {
                JsonRpcMessage::Notification(_) => {
                    eprintln!("notification: {message:?}");
                }
                JsonRpcMessage::Request(_) => {
                    anyhow::bail!("unexpected JSONRPCMessage::Request: {message:?}");
                }
                JsonRpcMessage::Response(ref resp) if resp.error.is_some() => {
                    anyhow::bail!("unexpected JSONRPCMessage error response: {message:?}");
                }
                JsonRpcMessage::Response(jsonrpc_response) => {
                    if jsonrpc_response.id == id_value {
                        return Ok(jsonrpc_response);
                    }
                }
            }
        }
    }

    pub async fn read_stream_until_response_or_error_message(
        &mut self,
        request_id: RequestId,
    ) -> anyhow::Result<JsonRpcMessage> {
        eprintln!("in read_stream_until_response_or_error_message({request_id:?})");
        let id_value = request_id.to_value();

        loop {
            let message = self.read_jsonrpc_message().await?;
            match message {
                JsonRpcMessage::Notification(_) => {
                    eprintln!("notification: {message:?}");
                }
                JsonRpcMessage::Request(_) => {
                    anyhow::bail!("unexpected JSONRPCMessage::Request: {message:?}");
                }
                JsonRpcMessage::Response(ref resp) if resp.id == id_value => {
                    return Ok(message);
                }
                JsonRpcMessage::Response(_) => {}
            }
        }
    }

    /// Reads notifications until a legacy TurnComplete event is observed:
    /// Method "codex/event" with params.msg.type == "task_complete".
    pub async fn read_stream_until_legacy_task_complete_notification(
        &mut self,
    ) -> anyhow::Result<JsonRpcRequest> {
        eprintln!("in read_stream_until_legacy_task_complete_notification()");

        loop {
            // Read only from the wire (not pending), buffering anything that is
            // not the task_complete notification so other read helpers can still
            // find it.
            let message = self.read_wire_message().await?;
            match message {
                JsonRpcMessage::Notification(notification) => {
                    let is_match = if notification.method == "codex/event" {
                        if let Some(params) = &notification.params {
                            params
                                .get("msg")
                                .and_then(|m| m.get("type"))
                                .and_then(|t| t.as_str())
                                == Some("task_complete")
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if is_match {
                        return Ok(notification);
                    } else {
                        eprintln!("ignoring notification: {notification:?}");
                    }
                }
                JsonRpcMessage::Request(_) => {
                    anyhow::bail!("unexpected JSONRPCMessage::Request: {message:?}");
                }
                JsonRpcMessage::Response(ref resp) if resp.error.is_some() => {
                    anyhow::bail!("unexpected JSONRPCMessage error response: {message:?}");
                }
                JsonRpcMessage::Response(_) => {
                    // A non-error Response (e.g. the tool call result) may race ahead of
                    // the task_complete notification. Buffer it so the next read helper
                    // can still find it.
                    eprintln!("buffering response while waiting for task_complete: {message:?}");
                    self.pending.push(message);
                }
            }
        }
    }

    /// Read exactly one message from the stdio wire (never from the pending buffer).
    async fn read_wire_message(&mut self) -> anyhow::Result<JsonRpcMessage> {
        let mut line = String::new();
        self.stdout.read_line(&mut line).await?;
        let message = serde_json::from_str::<JsonRpcMessage>(&line)?;
        eprintln!("read wire message: {message:?}");
        Ok(message)
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        // These tests spawn a `chaos mcp serve` child process.
        //
        // We keep that child alive for the test and rely on Tokio's `kill_on_drop(true)` when this
        // helper is dropped. Tokio documents kill-on-drop as best-effort: dropping requests
        // termination, but it does not guarantee the child has fully exited and been reaped before
        // teardown continues.
        //
        // That makes cleanup timing nondeterministic. Leak detection can occasionally observe the
        // child still alive at teardown and report `LEAK`, which makes the test flaky.
        //
        // Drop can't be async, so we do a bounded synchronous cleanup:
        //
        // 1. Request termination with `start_kill()`.
        // 2. Poll `try_wait()` until the OS reports the child exited, with a short timeout.
        let _ = self.process.start_kill();

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(5);
        while start.elapsed() < timeout {
            match self.process.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(_) => return,
            }
        }
    }
}
