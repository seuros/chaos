use super::*;
use chaos_kern::models_manager::catalog::ModelSelection;
use chaos_kern::test_support::test_model_info;

fn model_id(provider: &str, model: &str) -> String {
    ModelSelection {
        provider_id: provider.into(),
        model: model.into(),
    }
    .id()
}

fn option<'a>(response: &'a Value, id: &str) -> &'a Value {
    response["result"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == id)
        .unwrap()
}

async fn catalog_fixture() -> (Fixture, MockServer) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    let mut fixture = Fixture::new().await;
    let other = MockServer::start().await;
    fixture.config.model = Some("shared-model".into());
    fixture.config.model_catalog = None;
    let mut other_provider = fixture.config.model_provider.clone();
    other_provider.base_url = Some(format!("{}/v1", other.uri()));
    other_provider.experimental_bearer_token = Some("other-account-key".into());
    let mut locked = other_provider.clone();
    locked.experimental_bearer_token = None;
    locked.auth = Some(chaos_kern::ProviderAuthCapabilities {
        methods: vec![chaos_kern::ProviderAuthMethod::ApiKey],
    });
    fixture.config.model_providers = std::collections::HashMap::from([
        ("acp-fixture".into(), fixture.config.model_provider.clone()),
        ("other".into(), other_provider),
        ("locked".into(), locked),
    ]);
    for server in [&fixture.server, &other] {
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object":"list",
                "data":[
                    {"id":"shared-model","object":"model","owned_by":"fixture"},
                    {"id":"shared-model","object":"model","owned_by":"fixture"}
                ]
            })))
            .mount(server)
            .await;
    }
    (fixture, other)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_catalog_exposes_all_accounts_without_requiring_a_prompt() {
    let (fixture, _other) = catalog_fixture().await;
    let (mut peer, session) = fixture.peer().await;
    let original = model_id("acp-fixture", "shared-model");
    let selected = model_id("other", "shared-model");
    let options = option(&peer.initial, "model")["options"]
        .as_array()
        .unwrap();
    assert_eq!(options.len(), 2, "logged-out accounts must not appear");
    assert!(options.iter().any(|entry| entry["value"] == original));
    assert!(options.iter().any(|entry| entry["value"] == selected));
    let (changed, _) = peer
        .request(
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":selected}),
        )
        .await;
    assert_eq!(option(&changed, "model")["currentValue"], selected);
    peer.close().await;

    for method in ["session/load", "session/resume"] {
        let mut peer = Peer::connect(fixture.config.clone()).await;
        let (opened, _) = peer
            .request(
                method,
                json!({"sessionId":session,"cwd":fixture.cwd.path(),"mcpServers":[]}),
            )
            .await;
        assert!(opened["error"].is_null(), "{opened}");
        let models = option(&opened, "model");
        assert_eq!(models["currentValue"], selected);
        assert_eq!(models["options"].as_array().unwrap().len(), 2);
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_catalog_selects_and_restores_other_accounts_without_sending_picker_ids() {
    let (fixture, other) = catalog_fixture().await;
    let requests = mount_sse_sequence(
        &other,
        vec![
            response("other-r1", "other account"),
            response("other-r2", "restored account"),
        ],
    )
    .await;
    let (mut peer, session) = fixture.peer().await;
    let original = model_id("acp-fixture", "shared-model");
    let selected = model_id("other", "shared-model");
    let options = option(&peer.initial, "model")["options"]
        .as_array()
        .unwrap();
    assert_eq!(options.len(), 2, "logged-out accounts must not appear");
    assert!(options.iter().any(|entry| entry["value"] == original));
    assert!(options.iter().any(|entry| entry["value"] == selected));
    assert_eq!(option(&peer.initial, "model")["currentValue"], original);
    let (changed, _) = peer
        .request(
            "session/set_config_option",
            json!({
                "sessionId":session,"configId":"model","value":selected
            }),
        )
        .await;
    assert_eq!(
        option(&changed, "model")["currentValue"],
        selected,
        "{changed}"
    );
    let (rejected, _) = peer
        .request(
            "session/set_config_option",
            json!({
                "sessionId":session,"configId":"model","value":model_id("locked","shared-model")
            }),
        )
        .await;
    assert!(rejected["error"].is_object());
    let turn = peer.prompt(&session, "use the other account").await;
    peer.finish(&session, turn, "end_turn").await;
    assert_eq!(requests.requests()[0].body_json()["model"], "shared-model");
    let received = other.received_requests().await.unwrap();
    assert!(
        received
            .iter()
            .filter(|request| request.url.path() == "/v1/responses")
            .all(|request| request.headers.get("authorization").unwrap()
                == "Bearer other-account-key")
    );
    peer.close().await;

    let mut resumed = Peer::connect(fixture.config.clone()).await;
    let (loaded, _) = resumed
        .request(
            "session/load",
            json!({
                "sessionId":session,"cwd":fixture.cwd.path(),"mcpServers":[]
            }),
        )
        .await;
    assert!(loaded["error"].is_null(), "{loaded}");
    assert_eq!(option(&loaded, "model")["currentValue"], selected);
    assert_eq!(
        requests.requests().len(),
        1,
        "restoring controls never replays inference"
    );
    let turn = resumed
        .prompt(&session, "continue on the restored account")
        .await;
    resumed.finish(&session, turn, "end_turn").await;
    assert_eq!(requests.requests()[1].body_json()["model"], "shared-model");
    resumed.close().await;
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
    assert_eq!(
        option(&peer.initial, "model")["currentValue"],
        model_id("acp-fixture", "mock-model")
    );
    assert_eq!(peer.initial["result"]["modes"]["currentModeId"], "default");
    let (changed, updates) = peer
        .request(
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"mock-second"}),
        )
        .await;
    assert_eq!(
        option(&changed, "model")["currentValue"],
        model_id("acp-fixture", "mock-second")
    );
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
    assert_eq!(
        option(&second, "model")["currentValue"],
        model_id("acp-fixture", "mock-model")
    );
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
    assert!(
        updates
            .iter()
            .all(|frame| frame["params"]["update"]["sessionUpdate"] == "available_commands_update")
    );
    peer.close().await;
}
