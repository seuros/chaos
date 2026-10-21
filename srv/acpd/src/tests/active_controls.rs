use super::*;
use chaos_kern::test_support::test_model_info;
use core_test_support::responses::ev_apply_patch_custom_tool_call;
use core_test_support::responses::ev_function_call;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controls_change_during_a_pending_prompt_without_replaying_or_widening_policy() {
    let mut fixture = Fixture::new().await;
    fixture.supervised();
    fixture.config.model_catalog = Some(
        serde_json::from_value(json!({
            "models":[test_model_info("mock-model"),test_model_info("mock-second")]
        }))
        .unwrap(),
    );
    let (path, patch) = fixture.patch("active-control.txt", "must not run");
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![
            sse(vec![
                ev_response_created("active-control-r1"),
                ev_apply_patch_custom_tool_call("pending-edit", &patch),
                ev_completed("active-control-r1"),
            ]),
            response("active-control-r2", "next turn uses confirmed controls"),
        ],
    )
    .await;
    let (mut peer, session) = fixture.peer().await;
    let prompt = peer.prompt(&session, "propose an edit").await;
    let permission = peer.permission(&session).await;
    for (method, params) in [
        (
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"mock-second"}),
        ),
        (
            "session/set_mode",
            json!({"sessionId":session,"modeId":"plan"}),
        ),
    ] {
        let (response, _) = peer.request(method, params).await;
        assert!(response["result"].is_object(), "{response}");
    }
    for params in [
        json!({"sessionId":session,"configId":"model","value":"untrusted-provider/model"}),
        json!({"sessionId":session,"configId":"mode","value":"unknown"}),
    ] {
        let (response, _) = peer.request("session/set_config_option", params).await;
        assert!(response["error"].is_object(), "{response}");
    }
    peer.cancel(&session).await;
    peer.send(json!({"jsonrpc":"2.0","id":permission["id"],"result":{
        "outcome":{"outcome":"cancelled"}
    }}))
    .await;
    peer.finish(&session, prompt, "cancelled").await;
    assert_eq!(std::fs::read_to_string(path).unwrap(), "before\n");
    let prompt = peer
        .prompt(&session, "continue with the new controls")
        .await;
    peer.finish(&session, prompt, "end_turn").await;
    assert_eq!(
        requests.requests().len(),
        2,
        "no replay of the cancelled input"
    );
    let next = requests.requests()[1].body_json();
    assert_eq!(next["model"], "mock-second");
    assert!(next.to_string().contains("Plan Mode"));
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controls_take_effect_at_the_next_sample_of_the_same_prompt() {
    for compact in [false, true] {
        model_change_at_sampling_boundary(compact).await;
    }
}

async fn model_change_at_sampling_boundary(compact: bool) {
    let mut fixture = Fixture::new().await;
    let mut catalog =
        chaos_kern::test_support::test_models_response(&["mock-model", "mock-second"]);
    if compact {
        catalog.models[1].context_window = Some(16_000);
    }
    fixture.config.model_catalog = Some(catalog);
    let path = fixture.cwd.path().join("read-before-control.txt");
    std::fs::write(&path, "disk").unwrap();
    let mut responses = vec![sse(vec![
        ev_response_created("sample-r1"),
        ev_function_call("read", "read_file", &json!({"file_path":path}).to_string()),
        core_test_support::responses::ev_completed_with_tokens("sample-r1", 32_000),
    ])];
    if compact {
        responses.push(response(
            "distilled",
            "read finished; continue with new controls",
        ));
    }
    responses.push(response("sample-r2", "continued with new controls"));
    let requests = mount_sse_sequence(&fixture.server, responses).await;
    let mut peer =
        Peer::connect_capabilities(fixture.config.clone(), json!({"fs":{"readTextFile":true}}))
            .await;
    let (opened, _) = peer
        .request(
            "session/new",
            json!({"cwd":fixture.cwd.path(),"mcpServers":[]}),
        )
        .await;
    let id = opened["result"]["sessionId"].clone();
    let prompt = peer.prompt(&id, "read, then continue").await;
    let read = loop {
        let frame = peer.receive().await;
        if frame["method"] == "fs/read_text_file" {
            break frame;
        }
        assert_eq!(frame["method"], "session/update", "{frame}");
    };
    let (model, _) = peer
        .request(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"model","value":"mock-second"}),
        )
        .await;
    assert!(model["result"].is_object(), "{model}");
    let (mode, _) = peer
        .request("session/set_mode", json!({"sessionId":id,"modeId":"plan"}))
        .await;
    assert!(mode["result"].is_object(), "{mode}");
    peer.send(json!({"jsonrpc":"2.0","id":read["id"],"result":{"content":"editor text"}}))
        .await;
    peer.finish(&id, prompt, "end_turn").await;
    assert_eq!(
        requests.requests().len(),
        if compact { 3 } else { 2 },
        "control changes must not replay a sample"
    );
    assert_eq!(requests.requests()[0].body_json()["model"], "mock-model");
    if compact {
        assert_eq!(
            requests.requests()[1].body_json()["model"],
            "mock-model",
            "distill with the previous larger model before sampling the smaller one"
        );
    }
    let next = requests.requests().last().unwrap().body_json();
    assert_eq!(next["model"], "mock-second");
    assert!(next.to_string().contains("Plan Mode"));
    peer.close().await;
}
