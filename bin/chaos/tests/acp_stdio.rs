use std::process::Stdio;
use std::time::Duration;

use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_response_created, mount_sse_sequence, sse,
    start_mock_server,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct JournalFixture(tokio::task::JoinHandle<anyhow::Result<()>>);

impl Drop for JournalFixture {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn journal(home: &std::path::Path) -> (JournalFixture, std::path::PathBuf) {
    let config = chaos_journald::JournalServerConfig::from_chaos_home(home);
    let socket = config.socket_path.clone();
    let task = JournalFixture(tokio::spawn(chaos_journald::run_sqlite_journal_server(
        config,
    )));
    let client = chaos_journald::JournalRpcClient::new(socket.clone());
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if client.hello("acp-cli-test").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(ready.is_ok(), "isolated ACP journal startup: {ready:?}");
    (task, socket)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_stdio_is_protocol_only_and_exits_after_native_sessions_close() {
    let server = start_mock_server().await;
    let responses = ["CLI first response", "CLI continued response"]
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let id = format!("cli-r{index}");
            sse(vec![
                ev_response_created(&id),
                ev_assistant_message(&id, text),
                ev_completed(&id),
            ])
        })
        .collect();
    mount_sse_sequence(&server, responses).await;
    let home = TempDir::new_in("/tmp").unwrap();
    let cwd = TempDir::new().unwrap();
    let (_journal, journal_socket) = journal(home.path()).await;
    let catalog = home.path().join("models.json");
    std::fs::write(
        &catalog,
        serde_json::to_vec(&json!({
            "models":[chaos_kern::test_support::test_model_info("mock-model")]
        }))
        .unwrap(),
    )
    .unwrap();
    chaos_kern::test_support::seed_user_settings_toml(
        home.path(),
        &format!(
            r#"
model_provider = "acp-fixture"
cli_auth_credentials_store = "file"
model_catalog_json = {catalog:?}
[profiles.acp-fixture]
model = "mock-model"
[machine_warnings]
enabled = false
[model_providers.acp-fixture]
name = "Isolated ACP CLI fixture"
base_url = "{}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#,
            server.uri()
        ),
    )
    .await
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_chaos"))
        .args([
            "--provider",
            "acp-fixture",
            "--profile",
            "acp-fixture",
            "acp",
        ])
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
        .env("CHAOS_HOME", home.path())
        .env("CHAOS_JOURNALD_SOCKET", journal_socket)
        .env("RUST_LOG", "error")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut writer = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap()).lines();
    let deadline = Duration::from_secs(20);
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":1}});
    writer
        .write_all(format!("{initialize}\n").as_bytes())
        .await
        .unwrap();
    let initialized: Value = serde_json::from_str(
        &tokio::time::timeout(deadline, reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .expect("stdout must contain only ACP JSON frames");
    assert_eq!(initialized["result"]["agentInfo"]["name"], "chaos");
    assert_eq!(
        initialized["result"]["agentInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    let new = json!({"jsonrpc":"2.0","id":2,"method":"session/new",
        "params":{"cwd":cwd.path(),"mcpServers":[]}});
    writer
        .write_all(format!("{new}\n").as_bytes())
        .await
        .unwrap();
    let session: Value = serde_json::from_str(
        &tokio::time::timeout(deadline, reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let session = session["result"]["sessionId"].clone();
    assert!(session.is_string());
    for (id, expected) in [(3, "CLI first response"), (4, "CLI continued response")] {
        let prompt = json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
            "params":{"sessionId":session,"prompt":[{"type":"text","text":"fixture prompt"}]}});
        writer
            .write_all(format!("{prompt}\n").as_bytes())
            .await
            .unwrap();
        let mut text = String::new();
        loop {
            let frame: Value = serde_json::from_str(
                &tokio::time::timeout(deadline, reader.next_line())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
            )
            .expect("stdout must contain only ACP JSON frames");
            if frame["id"] == id {
                assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                break;
            }
            assert_eq!(frame["params"]["sessionId"], session);
            if frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                text.push_str(
                    frame["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap(),
                );
            }
        }
        assert_eq!(text, expected);
    }
    drop(writer);
    assert!(
        tokio::time::timeout(Duration::from_secs(6), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(reader.next_line().await.unwrap().is_none());
}
