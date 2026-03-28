#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_kern::CodexAuth;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::stdio_server_bin;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_with_timeout;
use tempfile::TempDir;
use wiremock::MockServer;

const SAMPLE_PLUGIN_CONFIG_NAME: &str = "sample@test";
const SAMPLE_PLUGIN_DISPLAY_NAME: &str = "sample";
const SAMPLE_PLUGIN_DESCRIPTION: &str = "inspect sample data";

fn sample_plugin_root(home: &TempDir) -> std::path::PathBuf {
    home.path().join("plugins/cache/test/sample/local")
}

fn write_sample_plugin_manifest_and_config(home: &TempDir) -> std::path::PathBuf {
    let plugin_root = sample_plugin_root(home);
    std::fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("create plugin manifest dir");
    std::fs::write(
        plugin_root.join(".codex-plugin/plugin.json"),
        format!(
            r#"{{"name":"{SAMPLE_PLUGIN_DISPLAY_NAME}","description":"{SAMPLE_PLUGIN_DESCRIPTION}"}}"#
        ),
    )
    .expect("write plugin manifest");
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[features]\nplugins = true\n\n[plugins.\"{SAMPLE_PLUGIN_CONFIG_NAME}\"]\nenabled = true\n"
        ),
    )
    .expect("write config");
    plugin_root
}

fn write_plugin_skill_plugin(home: &TempDir) -> std::path::PathBuf {
    let plugin_root = write_sample_plugin_manifest_and_config(home);
    let skill_dir = plugin_root.join("skills/sample-search");
    std::fs::create_dir_all(skill_dir.as_path()).expect("create plugin skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: inspect sample data\n---\n\n# body\n",
    )
    .expect("write plugin skill");
    skill_dir.join("SKILL.md")
}

fn write_plugin_mcp_plugin(home: &TempDir, command: &str) {
    let plugin_root = write_sample_plugin_manifest_and_config(home);
    std::fs::write(
        plugin_root.join(".mcp.json"),
        format!(
            r#"{{
  "mcpServers": {{
    "sample": {{
      "command": "{command}"
    }}
  }}
}}"#
        ),
    )
    .expect("write plugin mcp config");
}

async fn build_plugin_test_codex(
    server: &MockServer,
    codex_home: Arc<TempDir>,
) -> Result<Arc<chaos_kern::Process>> {
    let mut builder = test_codex()
        .with_home(codex_home)
        .with_auth(CodexAuth::from_api_key("Test API Key"));
    Ok(builder
        .build(server)
        .await
        .expect("create new conversation")
        .codex)
}

async fn build_analytics_plugin_test_codex(
    server: &MockServer,
    codex_home: Arc<TempDir>,
) -> Result<Arc<chaos_kern::Process>> {
    let chatgpt_base_url = server.uri();
    let mut builder = test_codex()
        .with_home(codex_home)
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_model("gpt-5")
        .with_config(move |config| {
            config.chatgpt_base_url = chatgpt_base_url;
        });
    Ok(builder
        .build(server)
        .await
        .expect("create new conversation")
        .codex)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_plugin_mentions_track_plugin_used_analytics() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let _resp_mock = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;

    let codex_home = Arc::new(TempDir::new()?);
    write_plugin_skill_plugin(codex_home.as_ref());
    let codex = build_analytics_plugin_test_codex(&server, codex_home).await?;

    codex
        .submit(Op::UserInput {
            items: vec![chaos_ipc::user_input::UserInput::Mention {
                name: "sample".into(),
                path: format!("plugin://{SAMPLE_PLUGIN_CONFIG_NAME}"),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let deadline = Instant::now() + Duration::from_secs(10);
    let analytics_request = loop {
        let requests = server.received_requests().await.unwrap_or_default();
        if let Some(request) = requests
            .into_iter()
            .find(|request| request.url.path() == "/codex/analytics-events/events")
        {
            break request;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for plugin analytics request");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let payload: serde_json::Value =
        serde_json::from_slice(&analytics_request.body).expect("analytics payload");
    let event = &payload["events"][0];
    assert_eq!(event["event_type"], "codex_plugin_used");
    assert_eq!(event["event_params"]["plugin_id"], "sample@test");
    assert_eq!(event["event_params"]["plugin_name"], "sample");
    assert_eq!(event["event_params"]["marketplace_name"], "test");
    assert_eq!(event["event_params"]["has_skills"], true);
    assert_eq!(event["event_params"]["mcp_server_count"], 0);
    assert_eq!(
        event["event_params"]["connector_ids"],
        serde_json::json!([])
    );
    assert_eq!(
        event["event_params"]["product_client_id"],
        serde_json::json!(chaos_kern::default_client::originator().value)
    );
    assert_eq!(event["event_params"]["model_slug"], "gpt-5");
    assert!(event["event_params"]["process_id"].as_str().is_some());
    assert!(event["event_params"]["turn_id"].as_str().is_some());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn plugin_mcp_tools_are_listed() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let codex_home = Arc::new(TempDir::new()?);
    let rmcp_test_server_bin = stdio_server_bin()?;
    write_plugin_mcp_plugin(codex_home.as_ref(), &rmcp_test_server_bin);
    let codex = build_plugin_test_codex(&server, codex_home).await?;

    let tools_ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        codex.submit(Op::ListMcpTools).await?;
        let list_event = wait_for_event_with_timeout(
            &codex,
            |ev| matches!(ev, EventMsg::McpListToolsResponse(_)),
            Duration::from_secs(10),
        )
        .await;
        let EventMsg::McpListToolsResponse(tool_list) = list_event else {
            unreachable!("event guard guarantees McpListToolsResponse");
        };
        if tool_list.tools.contains_key("mcp__sample__echo")
            && tool_list.tools.contains_key("mcp__sample__image")
        {
            break;
        }

        let available_tools: Vec<&str> = tool_list.tools.keys().map(String::as_str).collect();
        if Instant::now() >= tools_ready_deadline {
            panic!("timed out waiting for plugin MCP tools; discovered tools: {available_tools:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    Ok(())
}
