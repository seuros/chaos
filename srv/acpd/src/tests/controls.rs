use super::*;
use chaos_kern::test_support::test_model_info;

fn option<'a>(response: &'a Value, id: &str) -> &'a Value {
    response["result"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == id)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_mode_controls_are_native_confirmed_and_session_local() {
    let mut fixture = Fixture::new().await;
    fixture.config.model_catalog = Some(
        serde_json::from_value(json!({
            "models":[test_model_info("mock-model"),test_model_info("mock-second")]
        }))
        .unwrap(),
    );
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![response("control-r1", "selected model")],
    )
    .await;
    let (mut peer, session) = fixture.peer().await;
    assert_eq!(option(&peer.initial, "model")["currentValue"], "mock-model");
    assert_eq!(peer.initial["result"]["modes"]["currentModeId"], "default");
    let (changed, updates) = peer
        .request(
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"mock-second"}),
        )
        .await;
    assert_eq!(option(&changed, "model")["currentValue"], "mock-second");
    assert!(
        updates
            .iter()
            .any(|frame| frame["params"]["update"]["sessionUpdate"] == "config_option_update")
    );
    let (bad, _) = peer
        .request(
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"another-provider/model"}),
        )
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    let (bad, _) = peer
        .request(
            "session/set_config_option",
            json!({"sessionId":session,"configId":"provider","value":"untrusted"}),
        )
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    let (bad, _) = peer
        .request(
            "session/set_mode",
            json!({"sessionId":session,"modeId":"unknown"}),
        )
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    let (changed, updates) = peer
        .request(
            "session/set_mode",
            json!({"sessionId":session,"modeId":"plan"}),
        )
        .await;
    assert!(changed["result"].is_object(), "{changed}");
    assert!(
        updates
            .iter()
            .any(|frame| frame["params"]["update"]["currentModeId"] == "plan")
    );
    let (second, _) = peer
        .request(
            "session/new",
            json!({"cwd":fixture.cwd.path(),"mcpServers":[]}),
        )
        .await;
    assert_eq!(option(&second, "model")["currentValue"], "mock-model");
    assert_eq!(second["result"]["modes"]["currentModeId"], "default");
    let id = peer
        .prompt(&session, "use the confirmed model and mode")
        .await;
    peer.finish(&session, id, "end_turn").await;
    assert_eq!(requests.requests()[0].body_json()["model"], "mock-second");
    assert!(
        requests.requests()[0]
            .body_json()
            .to_string()
            .contains("Plan Mode")
    );
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_switching_cannot_raise_the_native_mode_ceiling() {
    let mut fixture = Fixture::new().await;
    chaos_kern::test_support::set_mode_policy_for_test(
        &mut fixture.config,
        "plan",
        &["plan"],
        false,
    );
    let (mut peer, session) = fixture.peer().await;
    assert_eq!(
        peer.initial["result"]["modes"]["availableModes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (bad, _) = peer
        .request(
            "session/set_mode",
            json!({"sessionId":session,"modeId":"default"}),
        )
        .await;
    assert!(bad["error"].is_object(), "{bad}");
    let (unchanged, _) = peer
        .request(
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"mock-model"}),
        )
        .await;
    assert_eq!(option(&unchanged, "mode")["currentValue"], "plan");
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_origin_mode_changes_update_the_picker_and_survive_restart() {
    use core_test_support::responses::ev_function_call;
    let fixture = Fixture::new().await;
    mount_sse_sequence(
        &fixture.server,
        vec![
            sse(vec![
                ev_response_created("native-mode-r1"),
                ev_function_call("native-mode", "switch_mode", r#"{"mode_id":"plan"}"#),
                ev_completed("native-mode-r1"),
            ]),
            response("native-mode-r2", "native mode switched"),
        ],
    )
    .await;
    let (mut peer, session) = fixture.peer().await;
    let prompt = peer
        .prompt(&session, "switch to planning with the native tool")
        .await;
    let updates = peer.finish(&session, prompt, "end_turn").await;
    assert!(
        updates
            .iter()
            .any(|frame| frame["params"]["update"]["currentModeId"] == "plan")
    );
    assert!(updates.iter().any(|frame| {
        frame["params"]["update"]["configOptions"]
            .as_array()
            .is_some_and(|options| {
                options
                    .iter()
                    .any(|option| option["id"] == "mode" && option["currentValue"] == "plan")
            })
    }));
    peer.close().await;
    let mut peer = Peer::connect(fixture.config.clone()).await;
    let (resumed, updates) = peer
        .request(
            "session/resume",
            json!({
                "sessionId":session,"cwd":fixture.cwd.path(),"mcpServers":[]
            }),
        )
        .await;
    assert!(resumed["error"].is_null(), "{resumed}");
    assert_eq!(resumed["result"]["modes"]["currentModeId"], "plan");
    assert!(updates.is_empty());
    peer.close().await;
}
