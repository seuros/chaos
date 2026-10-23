use std::time::Duration;

use chaos_kern::models_manager::catalog::ModelSelection;
use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_response_created, mount_sse_sequence, sse,
    start_mock_server,
};
use serde_json::json;
use tempfile::TempDir;

#[cfg(test)]
#[path = "acp_stdio/peer.rs"]
mod peer;
use peer::Peer;

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
    let responses = [
        "CLI first response",
        "CLI continued response",
        "CLI restored response",
    ]
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
    let requests = mount_sse_sequence(&server, responses).await;
    let home = TempDir::new_in("/tmp").unwrap();
    let cwd = TempDir::new().unwrap();
    let (_journal, journal_socket) = journal(home.path()).await;
    let catalog = home.path().join("models.json");
    std::fs::write(
        &catalog,
        serde_json::to_vec(&json!({
            "models":[
                chaos_kern::test_support::test_model_info("mock-model"),
                chaos_kern::test_support::test_model_info("mock-second")
            ]
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
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_chaos"));
    command
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
        .current_dir(cwd.path());
    let mut peer = Peer::open(&mut command).await;
    let (session, _) = peer
        .request(
            2,
            "session/new",
            json!({
                "cwd":cwd.path(),"mcpServers":[]
            }),
        )
        .await;
    let session = session["result"]["sessionId"].clone();
    assert!(session.is_string());
    let (selected, _) = peer
        .request(
            10,
            "session/set_config_option",
            json!({
                "sessionId":session,"configId":"model","value":"mock-second"
            }),
        )
        .await;
    assert!(selected["error"].is_null(), "{selected}");
    let selected_model = ModelSelection {
        provider_id: "acp-fixture".into(),
        model: "mock-second".into(),
    }
    .id();
    assert!(
        selected["result"]["configOptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["id"] == "model" && option["currentValue"] == selected_model),
        "{selected}"
    );
    let (mode, _) = peer
        .request(
            11,
            "session/set_mode",
            json!({
                "sessionId":session,"modeId":"plan"
            }),
        )
        .await;
    assert!(mode["error"].is_null(), "{mode}");
    peer.prompt(3, &session, "CLI first response").await;
    peer.prompt(4, &session, "CLI continued response").await;
    assert_eq!(requests.requests()[0].body_json()["model"], "mock-second");
    peer.close().await;

    // A new OS process reloads the existing native journal, not in-memory host state.
    let mut peer = Peer::open(&mut command).await;
    let (listed, _) = peer.request(2, "session/list", json!({})).await;
    assert!(
        listed["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["sessionId"] == session)
    );
    let (loaded, updates) = peer
        .request(
            3,
            "session/load",
            json!({
                "sessionId":session,"cwd":cwd.path(),"mcpServers":[]
            }),
        )
        .await;
    assert!(loaded["error"].is_null(), "{loaded}");
    assert_eq!(loaded["result"]["modes"]["currentModeId"], "plan");
    assert!(
        loaded["result"]["configOptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["id"] == "model" && option["currentValue"] == selected_model),
        "{loaded}"
    );
    let replay = serde_json::to_string(&updates).unwrap();
    assert!(replay.contains("CLI first response") && replay.contains("CLI continued response"));
    assert_eq!(
        requests.requests().len(),
        2,
        "load must not call the provider"
    );
    peer.prompt(4, &session, "CLI restored response").await;
    let continued = requests.requests()[2].body_json();
    assert_eq!(continued["model"], "mock-second");
    assert!(continued.to_string().contains("CLI first response"));
    peer.close().await;
}
