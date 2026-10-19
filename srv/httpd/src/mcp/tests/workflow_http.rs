use super::*;

#[tokio::test]
async fn http_workflows_tasks_and_apps_use_the_same_isolated_owner() {
    let (_home, mut state) = server_state(4096, false).await;
    let mut config = state.config.as_ref().clone();
    // No catalog discovery, model generation, commands or external services.
    config.clamp = true;
    let kernel = chaos_mcpd::build_server(
        Arc::new(config),
        Arg0DispatchPaths {
            alcatraz_exe: "/unused-test-alcatraz".into(),
        },
    )
    .await
    .unwrap();
    Arc::get_mut(&mut state).unwrap().mcp = Some(
        McpEndpoint::new(
            kernel,
            &cli(&["--mcp", "--body-limit", "4096"]),
            "test-token",
        )
        .unwrap(),
    );
    let service = crate::api::http_service(state.clone());
    let mut ids = Vec::new();
    for apps in [true, false] {
        let mut params = initialize();
        params["params"]["capabilities"] = json!({
            "tasks":{"requests":{"tools":{"call":{}}}}
        });
        if apps {
            params["params"]["capabilities"]["extensions"] = json!({
                "io.modelcontextprotocol/ui":{"mimeTypes":["text/html;profile=mcp-app"]}
            });
        }
        let response = service.serve(post(None, params)).await.unwrap();
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        answer(response).await;
        let initialized = service
            .serve(post(
                Some(&session),
                json!({
                    "jsonrpc":"2.0","method":"notifications/initialized"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(initialized.status(), StatusCode::ACCEPTED);
        ids.push(session);
    }
    let rpc = |session: String, method: &'static str, params: Value| {
        let service = service.clone();
        async move {
            answer(
                service
                    .serve(post(
                        Some(&session),
                        json!({
                            "jsonrpc":"2.0","id":10,"method":method,"params":params
                        }),
                    ))
                    .await
                    .unwrap(),
            )
            .await
        }
    };
    let enabled = rpc(
        ids[0].clone(),
        "tools/call",
        json!({
            "name":"enable_tools","arguments":{"groups":["workflows"]}
        }),
    )
    .await;
    assert_ne!(enabled["result"]["isError"], true, "{enabled}");
    let task = rpc(
        ids[0].clone(),
        "tools/call",
        json!({
            "name":"run_workflow",
            "arguments":{"tree":{
                "type":"condition","id":"check","pointer":"/input","predicate":{"op":"exists"}
            }},
            "task":{"ttl":60000}
        }),
    )
    .await;
    let task_id = task["result"]["task"]["taskId"].as_str().unwrap();
    let result = rpc(ids[0].clone(), "tasks/result", json!({"taskId":task_id})).await;
    assert_ne!(result["result"]["isError"], true, "{result}");
    let snapshot = &result["result"]["structuredContent"];
    assert_eq!(snapshot["status"]["outcome"], "success", "{result}");
    let run_id = snapshot["run_id"].as_str().unwrap();
    let dashboard = rpc(
        ids[0].clone(),
        "tools/call",
        json!({
            "name":"workflow_dashboard","arguments":{}
        }),
    )
    .await;
    assert_eq!(
        dashboard["result"]["structuredContent"]["runs"][0]["run_id"],
        run_id
    );
    let view = rpc(
        ids[0].clone(),
        "resources/read",
        json!({
            "uri":"ui://chaos/workflows"
        }),
    )
    .await;
    assert_eq!(
        view["result"]["contents"][0]["mimeType"],
        "text/html;profile=mcp-app"
    );
    for name in ["workflow_status", "cancel_workflow"] {
        let foreign = rpc(
            ids[1].clone(),
            "tools/call",
            json!({
                "name":name,"arguments":{"run_id":run_id}
            }),
        )
        .await;
        assert_eq!(foreign["result"]["isError"], true, "{foreign}");
    }
    let foreign_task = rpc(ids[1].clone(), "tasks/result", json!({"taskId":task_id})).await;
    assert!(foreign_task.get("error").is_some(), "{foreign_task}");
    let unsupported = rpc(
        ids[1].clone(),
        "tools/call",
        json!({
            "name":"workflow_dashboard","arguments":{}
        }),
    )
    .await;
    assert!(unsupported.get("error").is_some() || unsupported["result"]["isError"] == true);
    state.shutdown_sessions().await;
}
