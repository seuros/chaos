//! Real CLI/kernel/journald boundary; only the remote provider is simulated.
#![cfg(unix)]

use anyhow::{Context, Result, ensure};
use core_test_support::responses::{
    ev_assistant_message, ev_completed_with_tokens, ev_function_call, ev_response_created,
    ev_shell_command_call, sse,
};
use core_test_support::streaming_sse::{
    StreamingSseChunk, StreamingSseServer, start_streaming_sse_server,
};
use hmac::{Hmac, KeyInit, Mac};
use rama::Service;
use rama::http::{Body, Request, body::util::BodyExt};
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

const WAIT: Duration = Duration::from_secs(30);
const BEARER: &str = "daemon-integration-bearer";
const RECOVERY: &str = "daemon-integration-operator";
const SECRET: &str = "webhook-integration-signing-key-32-bytes";

struct Daemon {
    child: Child,
    journal: Child,
    _root: TempDir,
    home: PathBuf,
    work: PathBuf,
    env: HashMap<String, String>,
    settings: Vec<String>,
    port: u16,
    webhook: String,
    generation: String,
}

impl Daemon {
    async fn start(provider: &StreamingSseServer, webhook: String) -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("chaos-http-e2e-")
            .tempdir_in("/tmp")?;
        let home = root.path().join("home");
        let work = root.path().join("work");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&work)?;
        let home = home.canonicalize()?;
        let work = work.canonicalize()?;
        chaos_kern::test_support::bootstrap_sqlite(&home)?;
        chaos_kern::config::set_project_trust_level(
            &home,
            &work,
            chaos_ipc::config_types::TrustLevel::Trusted,
        )?;
        let catalog = home.join("models.json");
        std::fs::write(
            &catalog,
            serde_json::to_vec(&json!({
                "models": [chaos_kern::test_support::test_model_info(chaos_test_fixtures::TEST_MODEL)]
            }))?,
        )?;
        let env = HashMap::from([
            ("HOME".into(), root.path().to_string_lossy().into_owned()),
            ("CHAOS_HOME".into(), home.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                root.path().join("config").to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                root.path().join("cache").to_string_lossy().into_owned(),
            ),
            ("TMPDIR".into(), root.path().to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("SHELL".into(), "/bin/sh".into()),
            ("CHAOS_SESSION_WEBHOOK_SECRET".into(), SECRET.into()),
            ("NO_COLOR".into(), "1".into()),
        ]);
        let socket = home.join("run/journald.sock");
        let mut journal = Command::new(chaos_which::cargo_bin("chaos_journald")?)
            .env_clear()
            .envs(&env)
            .args(["--socket"])
            .arg(&socket)
            .arg("--db")
            .arg(home.join("chaos.sqlite"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let client = chaos_journald::JournalRpcClient::new(socket);
        tokio::time::timeout(WAIT, async {
            loop {
                if client.hello("httpd-e2e").await.is_ok() {
                    return Ok::<_, anyhow::Error>(());
                }
                ensure!(journal.try_wait()?.is_none(), "journald exited");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("journald readiness")??;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let settings = vec![
            "model_provider=\"http-e2e\"".into(),
            format!("model={:?}", chaos_test_fixtures::TEST_MODEL),
            format!(
                "model_providers.http-e2e={{name=\"HTTP E2E\",base_url=\"{}/v1\",wire_api=\"responses\",supports_websockets=false,request_max_retries=0,stream_max_retries=0}}",
                provider.uri()
            ),
            format!("model_catalog_json={:?}", catalog.to_string_lossy()),
            "cli_auth_credentials_store=\"ephemeral\"".into(),
            "analytics.enabled=false".into(),
            "machine_warnings.enabled=false".into(),
            "shell_environment_policy.inherit=\"core\"".into(),
        ];
        let child = Self::spawn(&env, &settings, &work, port, &webhook, false)?;
        let mut daemon = Self {
            child,
            journal,
            _root: root,
            home,
            work,
            env,
            settings,
            port,
            webhook,
            generation: String::new(),
        };
        daemon.ready().await?;
        Ok(daemon)
    }

    fn spawn(
        env: &HashMap<String, String>,
        settings: &[String],
        work: &Path,
        port: u16,
        webhook: &str,
        recovery_only: bool,
    ) -> Result<Child> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_chaos"));
        command.env_clear().envs(env).current_dir(work);
        for setting in settings {
            command.args(["-c", setting]);
        }
        command.args([
            "serve",
            "--sessions",
            "--session-scope",
            "integration",
            "--bearer-token",
            BEARER,
            "--recovery-token",
            RECOVERY,
            "--port",
            &port.to_string(),
            "--timeout",
            "300",
            "--skip-git-repo-check",
            "--sandbox",
            "root-access",
            "--session-webhook-url",
            webhook,
        ]);
        if recovery_only {
            command.arg("--session-recovery-only");
        }
        Ok(command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?)
    }

    async fn ready(&mut self) -> Result<()> {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(value) = self.api("GET", "/api/sessions", None, true).await {
                    self.generation = value["daemon_id"]
                        .as_str()
                        .context("daemon generation")?
                        .into();
                    return Ok::<_, anyhow::Error>(());
                }
                ensure!(
                    self.child.try_wait()?.is_none(),
                    "chaos serve exited before readiness"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("daemon readiness")??;
        Ok(())
    }

    async fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        operator: bool,
    ) -> Result<Value> {
        let client = rama::http::client::EasyHttpWebClient::default();
        let mut request = Request::builder()
            .method(method)
            .uri(format!("http://127.0.0.1:{}{path}", self.port))
            .header("authorization", format!("Bearer {BEARER}"))
            .header("x-chaos-daemon-id", &self.generation)
            .header("content-type", "application/json");
        if operator {
            request = request.header(
                "x-chaos-recovery-authorization",
                format!("Bearer {RECOVERY}"),
            );
        }
        let response = client
            .serve(request.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))?)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .to_bytes();
        ensure!(
            status.is_success(),
            "{method} {path}: {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn wait_status(&self, path: &str, object: &str, status: &str) -> Result<Value> {
        tokio::time::timeout(WAIT, async {
            loop {
                let value = self.api("GET", path, None, true).await?;
                if value[object]["status"] == status {
                    return Ok::<_, anyhow::Error>(value);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .with_context(|| format!("waiting for {path} {status}"))?
    }

    async fn restart_recovery_only(&mut self) -> Result<()> {
        self.child.kill().await?;
        self.child.wait().await?;
        // Do not forge ownership by rewriting expiry. A crash does not revoke
        // the dead daemon's 90-second lease; wait on the real storage clock.
        let pool = sqlx::SqlitePool::connect(&format!(
            "sqlite://{}",
            self.home.join("chaos.sqlite").display()
        ))
        .await?;
        let remaining: i64 = sqlx::query_scalar(
            "SELECT MAX(expires_at_ms) - CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) FROM execution_sessions"
        ).fetch_one(&pool).await?;
        pool.close().await;
        tokio::time::sleep(Duration::from_millis(remaining.max(0) as u64 + 100)).await;
        self.child = Self::spawn(
            &self.env,
            &self.settings,
            &self.work,
            self.port,
            &self.webhook,
            true,
        )?;
        self.ready().await
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.journal.start_kill();
    }
}

fn chunk(events: Vec<Value>, gate: Option<oneshot::Receiver<()>>) -> StreamingSseChunk {
    StreamingSseChunk {
        gate,
        body: sse(events),
    }
}

async fn wait_requests(provider: &StreamingSseServer, count: usize) -> Result<()> {
    tokio::time::timeout(WAIT, async {
        while provider.requests().await.len() < count {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("waiting for provider dispatch")?;
    Ok(())
}

struct ToolProcess(PathBuf);
impl ToolProcess {
    fn alive(&self) -> Result<bool> {
        let pid = std::fs::read_to_string(&self.0)?;
        Ok(std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()?
            .success())
    }
}

impl Drop for ToolProcess {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0) {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", pid.trim()])
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Receiver captures exact HTTP bytes, independent from the production sender.
async fn webhook_receiver() -> Result<(
    String,
    mpsc::Receiver<(String, Vec<u8>)>,
    tokio::task::JoinHandle<()>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                let parsed = loop {
                    let Ok(n) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(offset) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..offset]).into_owned();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= offset + 4 + length {
                            break (headers, bytes[offset + 4..offset + 4 + length].to_vec());
                        }
                    }
                };
                let _ = tx.send(parsed).await;
                let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            });
        }
    });
    Ok((format!("http://{address}/terminal"), rx, task))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_daemon_steering_receipts_webhook_and_sigkill_recovery() -> Result<()> {
    let _ = rama::tls::rustls::dep::rustls::crypto::ring::default_provider().install_default();
    let (release, gate) = oneshot::channel();
    let (_hold_crash_open, crash_gate) = oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![chunk(vec![
            ev_response_created("enable-shell"),
            ev_function_call("enable-shell-call", "enable_tools", r#"{"groups":["shell"]}"#),
            ev_completed_with_tokens("enable-shell", 3),
        ], None)],
        vec![
            chunk(vec![ev_response_created("tool-request")], None),
            chunk(
                vec![
                    ev_shell_command_call(
                        "safe-side-effect",
                        "test -z \"${CHAOS_SESSION_WEBHOOK_SECRET+x}\" && printf 'once\\n' >> side-effect.txt",
                    ),
                    ev_completed_with_tokens("tool-request", 7),
                ],
                Some(gate),
            ),
        ],
        vec![chunk(
            vec![
                ev_response_created("final-request"),
                ev_assistant_message("final-message", "STEERED_DONE"),
                ev_completed_with_tokens("final-request", 11),
            ],
            None,
        )],
        vec![chunk(
            vec![
                ev_response_created("subprocess-request"),
                ev_shell_command_call(
                    "sleeping-child",
                    "printf '%s' \"$$\" > child.pid; exec sleep 300",
                ),
                ev_completed_with_tokens("subprocess-request", 5),
            ],
            None,
        )],
        vec![
            chunk(vec![ev_response_created("crashed-request")], None),
            chunk(
                vec![ev_completed_with_tokens("crashed-request", 100)],
                Some(crash_gate),
            ),
        ],
    ])
    .await;
    let (url, mut events, receiver) = webhook_receiver().await?;
    let mut daemon = Daemon::start(&provider, url).await?;
    let created = daemon
        .api("POST", "/api/sessions", Some(json!({})), false)
        .await?;
    let session = created["session"]["session_id"]
        .as_str()
        .context("created session")?
        .to_owned();
    let session_path = format!("/api/sessions/{session}");
    daemon.wait_status(&session_path, "session", "idle").await?;
    let turn = "00000000-0000-4000-8000-000000000001";
    let turn_path = format!("{session_path}/turns/{turn}");
    daemon
        .api(
            "POST",
            &turn_path,
            Some(json!({"request":"run the harmless command"})),
            false,
        )
        .await?;
    wait_requests(&provider, 2).await?;
    daemon
        .api(
            "POST",
            &format!("{turn_path}/steer"),
            Some(json!({"request":"E2E_STEERING_MARKER"})),
            false,
        )
        .await?;
    release
        .send(())
        .map_err(|_| anyhow::anyhow!("provider gate closed"))?;
    daemon.wait_status(&turn_path, "turn", "completed").await?;
    let requests = provider.requests().await;
    let tool_output = requests
        .last()
        .and_then(|body| serde_json::from_slice::<Value>(body).ok())
        .map(|request| request["input"].clone());
    ensure!(
        std::fs::read_to_string(daemon.work.join("side-effect.txt")).with_context(|| format!(
            "shell did not produce side effect; native input: {tool_output:?}"
        ))? == "once\n",
        "tool side effect duplicated"
    );
    ensure!(requests.len() == 3, "unexpected provider invocations");
    ensure!(
        String::from_utf8_lossy(&requests[2]).contains("E2E_STEERING_MARKER"),
        "steering did not reach the next native sample"
    );
    let accounting = daemon
        .api("GET", &format!("{turn_path}/accounting"), None, false)
        .await?;
    ensure!(
        accounting["accounting"]["summary"]["known_usage"]["total_tokens"] == 21,
        "incorrect invocation receipts: {accounting}"
    );
    let (headers, body) = tokio::time::timeout(WAIT, events.recv())
        .await?
        .context("missing webhook")?;
    let event: Value = serde_json::from_slice(&body)?;
    ensure!(
        event["turn"]["turn_id"] == turn,
        "wrong terminal webhook: {event}"
    );
    ensure!(
        headers
            .to_ascii_lowercase()
            .contains("x-chaos-signature: v1="),
        "missing signed delivery"
    );
    let header = |name: &str| -> Result<&str> {
        headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name).then_some(value.trim())
            })
            .with_context(|| format!("missing {name}"))
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes())?;
    mac.update(header("x-chaos-timestamp")?.as_bytes());
    mac.update(b".");
    mac.update(header("x-chaos-event-id")?.as_bytes());
    mac.update(b".");
    mac.update(&body);
    let expected: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    ensure!(
        header("x-chaos-signature")? == format!("v1={expected}"),
        "incorrect webhook signature over exact bytes"
    );
    ensure!(
        !headers.to_ascii_lowercase().contains("authorization:"),
        "provider/control credentials leaked"
    );
    let interrupt_path = format!("{session_path}/turns/00000000-0000-4000-8000-000000000002");
    let tool = ToolProcess(daemon.work.join("child.pid"));
    daemon
        .api(
            "POST",
            &interrupt_path,
            Some(json!({"request":"start the bounded test child"})),
            false,
        )
        .await?;
    tokio::time::timeout(WAIT, async {
        while !tool.0.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("real shell process did not start")?;
    ensure!(
        tool.alive()?,
        "test child did not exist before interruption"
    );
    daemon
        .api(
            "POST",
            &format!("{interrupt_path}/interrupt"),
            Some(json!({})),
            false,
        )
        .await?;
    daemon
        .wait_status(&interrupt_path, "turn", "interrupted")
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while tool.alive()? {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("interrupted tool process survived native cleanup")??;
    std::fs::remove_file(&tool.0)?;
    let crash_turn = "00000000-0000-4000-8000-000000000003";
    let crash_path = format!("{session_path}/turns/{crash_turn}");
    daemon
        .api(
            "POST",
            &crash_path,
            Some(json!({"request":"hold this response until killed"})),
            false,
        )
        .await?;
    wait_requests(&provider, 5).await?;
    let generation = daemon.generation.clone();
    daemon.restart_recovery_only().await?;
    ensure!(
        daemon.generation != generation,
        "daemon generation survived process death"
    );
    let crashed = daemon.api("GET", &crash_path, None, true).await?;
    ensure!(
        crashed["turn"]["status"] == "unknown",
        "crash was incorrectly declared complete: {crashed}"
    );
    let usage = daemon
        .api("GET", &format!("{crash_path}/accounting"), None, true)
        .await?;
    ensure!(
        usage["accounting"]["summary"]["unknown_invocations"] == 1,
        "unacknowledged provider invocation was lost: {usage}"
    );
    let recovery_path = format!("{session_path}/recovery");
    let recovery = daemon.api("GET", &recovery_path, None, true).await?;
    ensure!(
        recovery["recovery"]["can_resolve"] == true,
        "dead supervisor not recoverable: {recovery}"
    );
    let record = &recovery["recovery"]["session"];
    let mut resolution = json!({
        "resolution_id": "00000000-0000-4000-8000-000000000004",
        "expected_process_id": record["process_id"],
        "expected_turn_id": record["active_turn_id"],
        "expected_recovery_epoch": record["recovery_epoch"],
        "containment_confirmed": false,
        "actor": "isolated-real-daemon-test",
        "reason": "SIGKILL reaped the daemon; the interrupted local tool was independently observed dead"
    });
    ensure!(
        daemon
            .api("POST", &recovery_path, Some(resolution.clone()), true)
            .await
            .is_err(),
        "resolution accepted absent containment confirmation"
    );
    resolution["containment_confirmed"] = json!(true);
    let resolved = daemon
        .api("POST", &recovery_path, Some(resolution.clone()), true)
        .await?;
    let repeated = daemon
        .api("POST", &recovery_path, Some(resolution), true)
        .await?;
    ensure!(
        resolved["resolution"] == repeated["resolution"],
        "resolution retry changed immutable audit"
    );
    ensure!(
        resolved["session"]["status"] == "closed",
        "recovery restarted rather than retired session"
    );
    let final_turn = daemon.api("GET", &crash_path, None, true).await?;
    ensure!(
        final_turn["turn"]["status"] == "interrupted",
        "resolution did not retire unknown turn"
    );
    let resolution_event = tokio::time::timeout(WAIT, async {
        loop {
            let (_, body) = events.recv().await.context("webhook receiver closed")?;
            let event: Value = serde_json::from_slice(&body)?;
            if event["turn"]["turn_id"] == crash_turn {
                return Ok::<_, anyhow::Error>(event);
            }
        }
    })
    .await
    .context("resolved terminal delivery after real restart")??;
    ensure!(
        resolution_event["accounting"]["unknown_invocations"] == 1,
        "recovery webhook concealed unknown usage: {resolution_event}"
    );
    ensure!(
        daemon
            .api(
                "POST",
                &format!("{session_path}/turns/00000000-0000-4000-8000-000000000005"),
                Some(json!({"request":"must not execute"})),
                true
            )
            .await
            .is_err(),
        "recovery-only mode admitted execution"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    ensure!(
        provider.requests().await.len() == 5,
        "restart replayed a provider request"
    );
    ensure!(
        std::fs::read_to_string(daemon.work.join("side-effect.txt"))? == "once\n",
        "restart repeated a real shell side effect"
    );
    daemon.child.kill().await?;
    daemon.child.wait().await?;
    daemon.journal.kill().await?;
    daemon.journal.wait().await?;
    receiver.abort();
    provider.shutdown().await;
    Ok(())
}
