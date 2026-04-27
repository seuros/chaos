#![allow(clippy::expect_used, clippy::unwrap_used)]

use chaos_kern::config::types::McpServerConfig;
use chaos_kern::config::types::McpServerTransportConfig;
use chaos_kern::config::upsert_global_mcp_server;
use core_test_support::responses;
use core_test_support::test_chaos_fork::test_chaos_fork;
use predicates::str::contains;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exits_non_zero_when_required_mcp_server_fails_to_initialize() -> anyhow::Result<()> {
    let test = test_chaos_fork();

    upsert_global_mcp_server(
        test.home_path(),
        "required_broken",
        &McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "chaos-definitely-not-a-real-binary".to_string(),
                args: Vec::new(),
                env: None,
                env_vars: Vec::new(),
                cwd: None,
            },
            enabled: true,
            required: true,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth_resource: None,
            r#type: None,
            oauth: None,
        },
    )
    .await?;

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp_1"),
        responses::ev_assistant_message("msg_1", "hello"),
        responses::ev_completed("resp_1"),
    ]);
    responses::mount_sse_once(&server, body).await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--experimental-json")
        .arg("tell me something")
        .assert()
        .code(1)
        .stderr(contains(
            "required MCP servers failed to initialize: required_broken",
        ));

    Ok(())
}
