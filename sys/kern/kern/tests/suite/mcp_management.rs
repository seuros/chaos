use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use chaos_ipc::config_types::TrustLevel;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::McpServerRefreshConfig;
use chaos_ipc::protocol::Op;
use chaos_kern::config::load_global_mcp_servers;
use chaos_kern::config::set_project_trust_level;
use chaos_kern::config::types::McpServerConfig;
use chaos_kern::config::upsert_global_mcp_server;
use core_test_support::process::process_is_alive;
use core_test_support::process::wait_for_process_exit;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::stdio_server_bin;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use serde_json::Value;
use serde_json::json;
use serial_test::serial;
use tempfile::tempdir;

fn advertises_server(body: &Value, server: &str) -> bool {
    let prefix = format!("mcp__{server}__");
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["name"].as_str())
        .any(|name| name.starts_with(&prefix))
}

fn management_responses(calls: &[(&str, &str, Value)]) -> Vec<String> {
    let mut bodies = calls
        .iter()
        .map(|(call_id, name, args)| {
            responses::sse(vec![
                responses::ev_response_created(call_id),
                responses::ev_function_call(call_id, name, &args.to_string()),
                responses::ev_completed(call_id),
            ])
        })
        .collect::<Vec<_>>();
    bodies.push(responses::sse(vec![
        responses::ev_assistant_message("done", "MCP tools updated without restarting."),
        responses::ev_completed("done"),
    ]));
    bodies
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[serial(process_env)]
async fn management_updates_tools_in_same_turn_despite_optional_startup_failure()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let fixture = test_chaos().build(&server).await?;
    set_project_trust_level(
        fixture.chaos_home_path(),
        fixture.cwd_path(),
        TrustLevel::Trusted,
    )?;
    let echo_tool = "mcp__live__echo";
    let calls = [
        (
            "enable-management",
            "enable_tools",
            json!({"groups": ["mcp-management"]}),
        ),
        (
            "add-unavailable",
            "mcp_add_server",
            json!({
                "name": "unavailable",
                "command": fixture.cwd_path().join("missing-mcp-server"),
            }),
        ),
        (
            "add-live",
            "mcp_add_server",
            json!({"name": "live", "command": stdio_server_bin()?}),
        ),
        ("echo-live", echo_tool, json!({"message": "ping"})),
        (
            "disable-live",
            "mcp_server",
            json!({"name": "live", "action": "disable"}),
        ),
        (
            "enable-live",
            "mcp_server",
            json!({"name": "live", "action": "enable"}),
        ),
        (
            "remove-live",
            "mcp_server",
            json!({"name": "live", "action": "remove"}),
        ),
        ("echo-removed", echo_tool, json!({"message": "stale call"})),
        (
            "read-state",
            "read_mcp_resource",
            json!({"uri": "chaos://mcp"}),
        ),
    ];
    let mock = responses::mount_sse_sequence(&server, management_responses(&calls)).await;

    fixture
        .submit_turn("Add, call, disable, enable, and remove the live MCP server.")
        .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), calls.len() + 1);
    for (index, request) in requests.iter().enumerate() {
        let body = request.body_json();
        assert_eq!(
            advertises_server(&body, "live"),
            matches!(index, 3 | 4 | 6),
            "unexpected live MCP tool surface in sample {index}"
        );
        assert!(!advertises_server(&body, "unavailable"));
    }
    assert!(
        mock.function_call_output_text("echo-live")
            .context("missing live echo result")?
            .contains("ECHOING: ping")
    );
    assert_eq!(
        mock.function_call_output_text("echo-removed")
            .context("missing removed echo result")?,
        format!("unsupported call: {echo_tool}")
    );

    let resource: Value = serde_json::from_str(
        &mock
            .function_call_output_text("read-state")
            .context("missing MCP resource result")?,
    )?;
    let state: Value = serde_json::from_str(
        resource["contents"][0]["text"]
            .as_str()
            .context("missing MCP state")?,
    )?;
    assert_eq!(state["revision"], 5);
    let servers = state["servers"].as_array().context("missing MCP servers")?;
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["name"], "unavailable");
    assert_eq!(servers[0]["status"]["state"], "failed");
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[serial(process_env)]
async fn reset_reconnects_only_the_effective_global_or_project_server() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let _credential_store = chaos_keyring::tests::MockKeyringStore::default();

    for project_override in [false, true] {
        let server = responses::start_mock_server().await;
        let home = Arc::new(tempdir()?);
        chaos_kern::test_support::bootstrap_sqlite(home.path())?;
        let logs = tempdir()?;
        let target_log = logs.path().join("target-pids");
        let other_log = logs.path().join("other-pids");
        let command = stdio_server_bin()?;
        for (name, log) in [("target", &target_log), ("other", &other_log)] {
            let config = serde_json::from_value(json!({
                "command": command,
                "env": {
                    "MCP_TEST_INCLUDE_CLIENT_ID": "1",
                    "MCP_TEST_VALUE": "global",
                    "MCP_TEST_PID_FILE": log,
                },
            }))?;
            upsert_global_mcp_server(home.path(), name, &config).await?;
        }
        for (name, enabled) in [("disabled", false), ("unavailable", true)] {
            let config = serde_json::from_value(json!({
                "command": logs.path().join("missing-server"),
                "enabled": enabled,
            }))?;
            upsert_global_mcp_server(home.path(), name, &config).await?;
        }
        let globals_before = load_global_mcp_servers(home.path()).await?;
        let fixture = test_chaos().with_home(home).build(&server).await?;
        set_project_trust_level(
            fixture.chaos_home_path(),
            fixture.cwd_path(),
            TrustLevel::Trusted,
        )?;
        let mut setup_calls = vec![(
            "enable-management",
            "enable_tools",
            json!({"groups": ["mcp-management"]}),
        )];
        if project_override {
            setup_calls.push((
                "override-target",
                "mcp_add_server",
                json!({
                    "name": "target",
                    "command": command,
                    "env": {
                        "MCP_TEST_INCLUDE_CLIENT_ID": "1",
                        "MCP_TEST_VALUE": "project",
                        "MCP_TEST_PID_FILE": target_log,
                    },
                }),
            ));
        }
        setup_calls.extend([
            (
                "target-before",
                "mcp__target__echo",
                json!({"message": "before"}),
            ),
            (
                "other-before",
                "mcp__other__echo",
                json!({"message": "before"}),
            ),
        ]);
        let reset_calls = [
            (
                "reset-target",
                "mcp_server",
                json!({"name": "target", "action": "reset"}),
            ),
            (
                "target-after",
                "mcp__target__echo",
                json!({"message": "after"}),
            ),
            (
                "reset-again",
                "mcp_server",
                json!({"name": "target", "action": "reset"}),
            ),
            (
                "target-again",
                "mcp__target__echo",
                json!({"message": "again"}),
            ),
            (
                "reset-disabled",
                "mcp_server",
                json!({"name": "disabled", "action": "reset"}),
            ),
            (
                "reset-unknown",
                "mcp_server",
                json!({"name": "unknown", "action": "reset"}),
            ),
            (
                "reset-failed",
                "mcp_server",
                json!({"name": "unavailable", "action": "reset"}),
            ),
            (
                "other-after",
                "mcp__other__echo",
                json!({"message": "after"}),
            ),
            (
                "read-state",
                "read_mcp_resource",
                json!({"uri": "chaos://mcp"}),
            ),
        ];
        let mut bodies = management_responses(&setup_calls);
        bodies.extend(management_responses(&reset_calls));
        let mock = responses::mount_sse_sequence(&server, bodies).await;
        fixture
            .submit_turn("Read the configured MCP client identities.")
            .await?;
        let target_before = std::fs::read_to_string(&target_log)?;
        let other_before = std::fs::read_to_string(&other_log)?;
        let revision_before = fixture.process.mcp_resource_servers().0;
        let project_path = fixture.cwd_path().join(".mcp.json");
        let project_before = std::fs::read(&project_path).ok();
        fixture
            .submit_turn("Reset only target twice, then check invalid resets and other.")
            .await?;

        let output = |id| -> anyhow::Result<String> {
            mock.function_call_output_text(id)
                .context("missing tool result")
        };
        let echo = |id| -> anyhow::Result<Value> { Ok(serde_json::from_str(&output(id)?)?) };
        let before = echo("target-before")?;
        for id in ["target-after", "target-again"] {
            let after = echo(id)?;
            assert_eq!(after["client_id"], before["client_id"]);
            assert_eq!(
                after["env"],
                if project_override {
                    "project"
                } else {
                    "global"
                }
            );
        }
        assert_eq!(
            echo("other-before")?["client_id"],
            echo("other-after")?["client_id"]
        );
        for id in ["reset-target", "reset-again"] {
            let result: Value = serde_json::from_str(&output(id)?)?;
            assert_eq!(result["status"], "reset");
            assert_eq!(result["scope"], "session");
            assert!(result.get("path").is_none());
        }
        assert!(output("reset-disabled")?.contains("is disabled"));
        assert!(output("reset-unknown")?.contains("unknown MCP server"));
        assert!(output("reset-failed")?.contains("failed to reset MCP server"));
        let target_after = std::fs::read_to_string(&target_log)?;
        assert_eq!(
            target_after.lines().count(),
            target_before.lines().count() + 2
        );
        assert_eq!(std::fs::read_to_string(&other_log)?, other_before);
        let retired_pid = target_before.lines().last().context("target PID")?;
        wait_for_process_exit(retired_pid).await?;
        assert!(process_is_alive(
            other_before.lines().last().context("other PID")?
        )?);
        assert_eq!(std::fs::read(&project_path).ok(), project_before);
        assert_eq!(
            load_global_mcp_servers(fixture.chaos_home_path()).await?,
            globals_before
        );
        let resource: Value = serde_json::from_str(&output("read-state")?)?;
        let state: Value = serde_json::from_str(
            resource["contents"][0]["text"]
                .as_str()
                .context("MCP state")?,
        )?;
        assert_eq!(state["revision"], revision_before + 3);
        for request in mock.requests() {
            let body = request.body_json();
            assert!(advertises_server(&body, "other"));
            assert!(!advertises_server(&body, "disabled"));
        }
        server.verify().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[serial(process_env)]
async fn required_startup_failure_preserves_active_tools() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let initial_servers: HashMap<String, McpServerConfig> =
        serde_json::from_value(json!({"live": {"command": stdio_server_bin()?}}))?;
    let fixture = test_chaos()
        .with_config(move |config| {
            config
                .mcp_servers
                .set(initial_servers)
                .expect("test MCP configuration");
        })
        .build(&server)
        .await?;
    fixture
        .process
        .submit(Op::RefreshMcpServers {
            config: McpServerRefreshConfig {
                mcp_servers: json!({
                    "required": {
                        "command": fixture.cwd_path().join("missing-mcp-server"),
                        "required": true,
                    },
                }),
                mcp_oauth_credentials_store_mode: serde_json::to_value(
                    fixture.config.mcp_oauth_credentials_store_mode,
                )?,
            },
        })
        .await?;
    let event = wait_for_event(&fixture.process, |event| {
        matches!(event, EventMsg::McpServersRefreshed(_))
    })
    .await;
    let EventMsg::McpServersRefreshed(refreshed) = event else {
        unreachable!("waited for MCP refresh");
    };
    assert!(!refreshed.applied);
    assert_eq!(refreshed.revision, 0);
    assert!(refreshed.added.is_empty());
    assert!(refreshed.removed.is_empty());
    assert_eq!(refreshed.failed.len(), 1);
    assert_eq!(refreshed.failed[0].server, "required");
    assert_eq!(
        fixture.process.mcp_resource_servers(),
        (0, vec!["live".to_string()])
    );

    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_function_call(
                    "echo-preserved",
                    "mcp__live__echo",
                    r#"{"message":"still live"}"#,
                ),
                responses::ev_completed("echo-preserved"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("done", "Previous MCP generation preserved."),
                responses::ev_completed("done"),
            ]),
        ],
    )
    .await;
    fixture.submit_turn("Call the preserved echo tool.").await?;
    assert!(
        mock.function_call_output_text("echo-preserved")
            .context("missing preserved echo result")?
            .contains("ECHOING: still live")
    );
    for request in mock.requests() {
        assert!(advertises_server(&request.body_json(), "live"));
        assert!(!advertises_server(&request.body_json(), "required"));
    }
    server.verify().await;
    Ok(())
}
