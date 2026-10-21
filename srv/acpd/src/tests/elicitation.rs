use super::*;
use core_test_support::responses::ev_function_call;

#[test]
fn session_owned_mcp_elicitations_are_not_filtered_by_prompt_submission_id() {
    // The full MCP HTTP/resource dispatch graph carries large debug futures.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()
        .unwrap()
        .block_on(Box::pin(mcp_elicitation_round_trip()));
}

async fn mcp_elicitation_round_trip() {
    for mode in ["form", "url"] {
        let mut fixture = Fixture::new().await;
        fixture.supervised();
        let mcp = MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |request: &wiremock::Request| {
                let request: Value = serde_json::from_slice(&request.body).unwrap();
                let result = match request["method"].as_str() {
                    Some("initialize") => json!({
                        "protocolVersion":"2025-11-25",
                        "capabilities":{"tools":{},"resources":{}},
                        "serverInfo":{"name":"elicitor","version":"1"}
                    }),
                    Some("resources/list") => json!({"resources":[]}),
                    Some("resources/templates/list") => json!({"resourceTemplates":[]}),
                    Some("tools/list") => {
                        let params = if mode == "form" {
                            json!({"mode":"form","message":"MCP consent","requestedSchema":{
                                "type":"object","properties":{"ok":{"type":"boolean"}},
                                "required":["ok"]
                            }})
                        } else {
                            json!({"mode":"url","message":"MCP consent",
                                "elicitationId":"external","url":"https://example.invalid"})
                        };
                        let ask = json!({"jsonrpc":"2.0","id":"consent",
                            "method":"elicitation/create","params":params});
                        let listed = json!({"jsonrpc":"2.0","id":request["id"],
                            "result":{"tools":[]}});
                        return wiremock::ResponseTemplate::new(200).set_body_raw(
                            format!(
                                "event: message\ndata: {ask}\n\nevent: message\ndata: {listed}\n\n"
                            ),
                            "text/event-stream",
                        );
                    }
                    _ => return wiremock::ResponseTemplate::new(202),
                };
                wiremock::ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc":"2.0","id":request["id"],"result":result
                }))
            })
            .mount(&mcp)
            .await;
        fixture
            .config
            .mcp_servers
            .set(
                serde_json::from_value(json!({
                    "elicitor":{"url":mcp.uri(),"enabled":true}
                }))
                .unwrap(),
            )
            .unwrap();
        let requests = mount_sse_sequence(
            &fixture.server,
            vec![
                files::tool_response(ev_function_call(
                    "list",
                    "list_mcp_resources",
                    r#"{"server":"elicitor"}"#,
                )),
                response("mcp-r1", "consent handled"),
            ],
        )
        .await;
        let mut peer =
            Peer::connect_capabilities(fixture.config.clone(), json!({"elicitation":{(mode):{}}}))
                .await;
        let (created, _) = peer
            .request(
                "session/new",
                json!({"cwd":fixture.cwd.path(),"mcpServers":[]}),
            )
            .await;
        let id = created["result"]["sessionId"].clone();
        let prompt = peer.prompt(&id, "handle the MCP request").await;
        let mut elicited = 0;
        loop {
            let frame = peer.receive().await;
            if frame["id"] == prompt {
                assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                break;
            }
            if frame["method"] == "elicitation/create" {
                elicited += 1;
                assert_eq!(frame["params"]["sessionId"], id);
                assert_eq!(frame["params"]["mode"], mode);
                peer.send(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"action":"decline"}}))
                    .await;
            } else {
                assert_eq!(frame["method"], "session/update", "{frame}");
            }
        }
        assert_eq!(
            elicited,
            1,
            "{mode}: {}",
            requests.requests().last().unwrap().body_json()["input"]
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if mcp
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .any(|request| {
                        let value: Value = serde_json::from_slice(&request.body).unwrap();
                        value["id"] == "consent" && value["result"]["action"] == "decline"
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_user_input_uses_negotiated_forms_without_collecting_secrets() {
    for (supported, secret) in [(true, false), (false, false), (true, true)] {
        let fixture = Fixture::new().await;
        let requests = mount_sse_sequence(
            &fixture.server,
            vec![
                sse(vec![
                    ev_response_created("form-r1"),
                    ev_function_call(
                        "choose",
                        "request_user_input",
                        &json!({"questions":[{
                            "id":"direction","header":"Direction","question":"Continue?",
                            "isOther":false,"isSecret":secret,"options":[
                                {"label":"Yes","description":"Continue"},
                                {"label":"No","description":"Stop"}
                            ]
                        }]})
                        .to_string(),
                    ),
                    ev_completed("form-r1"),
                ]),
                response("form-r2", "resolved"),
            ],
        )
        .await;
        let caps = if supported {
            json!({"elicitation":{"form":{}}})
        } else {
            json!({})
        };
        let mut peer = Peer::connect_capabilities(fixture.config.clone(), caps).await;
        let (created, _) = peer
            .request(
                "session/new",
                json!({"cwd":fixture.cwd.path(),"mcpServers":[]}),
            )
            .await;
        let session = created["result"]["sessionId"].clone();
        let (mode, _) = peer
            .request(
                "session/set_mode",
                json!({"sessionId":session,"modeId":"plan"}),
            )
            .await;
        assert!(mode["result"].is_object(), "{mode}");
        let prompt = peer.prompt(&session, "ask before proceeding").await;
        let mut elicited = 0;
        loop {
            let frame = peer.receive().await;
            if frame["id"] == prompt {
                assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                break;
            }
            if frame["method"] == "elicitation/create" {
                elicited += 1;
                assert!(supported && !secret);
                assert_eq!(frame["params"]["sessionId"], session);
                let field = &frame["params"]["requestedSchema"]["properties"]["direction"];
                assert_eq!(field["type"], "string");
                assert!(
                    field["description"]
                        .as_str()
                        .unwrap()
                        .contains("Yes: Continue")
                );
                // Native questions always allow an additional free-form answer.
                assert!(field.get("enum").is_none());
                peer.send(json!({"jsonrpc":"2.0","id":frame["id"],"result":{
                    "action":"accept","content":{"direction":"Yes"}
                }}))
                .await;
            } else {
                assert_eq!(frame["method"], "session/update", "{frame}");
            }
        }
        assert_eq!(elicited, usize::from(supported && !secret));
        assert_eq!(requests.requests().len(), 2);
        if supported && !secret {
            assert!(
                requests.requests()[1]
                    .body_json()
                    .to_string()
                    .contains("Yes")
            );
        }
        peer.close().await;
    }
}
