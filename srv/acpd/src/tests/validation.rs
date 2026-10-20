use super::*;
use acp_host::protocol::schema::v1::McpServer;
use chaos_kern::config::types::McpServerConfig;
use core_test_support::responses::ev_function_call;

#[tokio::test]
async fn client_mcp_is_only_a_selector_for_registered_configuration() {
    let home = TempDir::new().unwrap();
    let mut config = core_test_support::load_default_config_for_test(&home).await;
    let registered: McpServerConfig = serde_json::from_value(json!({
        "command":"/registered/mcp-fixture","args":["--stdio"],
        "env":{"FIXTURE":"registered-value"}
    }))
    .unwrap();
    config
        .mcp_servers
        .set([("fixture".into(), registered)].into())
        .unwrap();
    let valid = json!({"name":"fixture","command":"/registered/mcp-fixture",
        "args":["--stdio"],"env":[{"name":"FIXTURE","value":"registered-value"}]});
    let decode = |value| serde_json::from_value::<McpServer>(value).unwrap();
    assert!(crate::config::validate_mcp(&config, &[decode(valid.clone())]).is_ok());
    let mut without_env = valid.clone();
    without_env["env"] = json!([]);
    assert!(crate::config::validate_mcp(&config, &[decode(without_env)]).is_ok());
    for (field, changed) in [
        ("name", json!("unregistered")),
        ("command", json!("/untrusted/execute")),
        ("args", json!(["--different"])),
        ("env", json!([{"name":"FIXTURE","value":"injected"}])),
        ("env", json!([{"name":"OTHER","value":"injected"}])),
        (
            "env",
            json!([{"name":"FIXTURE","value":"registered-value"},
            {"name":"FIXTURE","value":"registered-value"}]),
        ),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = changed;
        assert!(crate::config::validate_mcp(&config, &[decode(invalid)]).is_err());
    }
    assert!(crate::config::validate_mcp(&config, &[decode(valid.clone()), decode(valid)]).is_err());
    assert!(
        crate::config::validate_mcp(
            &config,
            &[decode(json!({
                "type":"http","name":"fixture","url":"https://untrusted.invalid","headers":[]
            }))]
        )
        .is_err()
    );
    let mut disabled = config.mcp_servers.get().clone();
    disabled.get_mut("fixture").unwrap().enabled = false;
    config.mcp_servers.set(disabled).unwrap();
    assert!(
        crate::config::validate_mcp(
            &config,
            &[decode(json!({
                "name":"fixture","command":"/registered/mcp-fixture","args":["--stdio"],"env":[]
            }))]
        )
        .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_file_read_is_visible_as_a_tool_not_editor_privileged_io() {
    let fixture = Fixture::new().await;
    let file = fixture.cwd.path().join("private-read-fixture.txt");
    std::fs::write(&file, "native-read-marker-91837\n").unwrap();
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![
            sse(vec![
                ev_response_created("read-r1"),
                ev_function_call(
                    "private-read",
                    "read_file",
                    &json!({"file_path":file}).to_string(),
                ),
                ev_completed("read-r1"),
            ]),
            response("read-r2", "read complete"),
        ],
    )
    .await;
    let (mut peer, session) = fixture.peer().await;
    let prompt = peer.prompt(&session, "read the private fixture").await;
    let mut saw_result = false;
    let mut saw_begin = false;
    for frame in peer.finish(&session, prompt, "end_turn").await {
        let update = &frame["params"]["update"];
        saw_begin |=
            update["sessionUpdate"] == "tool_call" && update["toolCallId"] == "private-read";
        saw_result |= update["sessionUpdate"] == "tool_call_update"
            && update["status"] == "completed"
            && update.to_string().contains("native-read-marker-91837");
    }
    assert!(saw_begin && saw_result);
    assert!(
        requests.requests()[1]
            .body_json()
            .to_string()
            .contains("native-read-marker-91837")
    );
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_reference_is_user_context_not_an_automatic_fetch() {
    let fixture = Fixture::new().await;
    let requests =
        mount_sse_sequence(&fixture.server, vec![response("resource-r1", "noted")]).await;
    let (mut peer, session) = fixture.peer().await;
    peer.send(
        json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{
        "sessionId":session,"prompt":[{"type":"resource_link","uri":"file:///untrusted/reference",
            "name":"private reference"}]}}),
    )
    .await;
    peer.finish(&session, 3, "end_turn").await;
    let body = requests.requests()[0].body_json();
    let user: Vec<_> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user")
        .collect();
    assert!(user.iter().any(|item| {
        item.to_string()
            .contains("Referenced resource (untrusted context): file:///untrusted/reference")
    }));
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_provider_credentials_are_explicit_and_never_replayed() {
    let mut fixture = Fixture::new().await;
    fixture.config.model_provider.env_key = Some("CHAOS_ACP_ISOLATED_NONEXISTENT_KEY_91837".into());
    let (mut peer, session) = fixture.peer().await;
    let prompt = peer.prompt(&session, "must not reach a provider").await;
    loop {
        let frame = peer.receive().await;
        if frame["id"] == prompt {
            assert!(frame["error"].is_object(), "{frame}");
            assert!(
                frame["error"]["data"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("Configure the selected provider"),
                "{frame}"
            );
            break;
        }
        assert_eq!(frame["method"], "session/update", "{frame}");
    }
    let next = peer.prompt(&session, "do not replay uncertain work").await;
    let frame = peer.receive().await;
    assert_eq!(frame["id"], next);
    assert!(frame["error"].is_object());
    assert!(
        fixture
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "POST")
    );
    peer.close().await;
}
