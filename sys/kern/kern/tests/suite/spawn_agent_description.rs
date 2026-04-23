#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::Result;
use chaos_ipc::openai_models::ModelVisibility;
use chaos_ipc::openai_models::ModelsResponse;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::openai_models::ReasoningEffortPreset;
use chaos_kern::ChaosAuth;
use chaos_kern::test_support::test_remote_model;
use chaos_kern::test_support::wait_for_model_available;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_models_once;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_chaos::test_chaos;
use serde_json::Value;

const SPAWN_AGENT_TOOL_NAME: &str = "spawn_agent";

fn spawn_agent_description(body: &Value) -> Option<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .and_then(|tools| {
            tools.iter().find_map(|tool| {
                if tool.get("name").and_then(Value::as_str) == Some(SPAWN_AGENT_TOOL_NAME) {
                    tool.get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                } else {
                    None
                }
            })
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_agent_description_lists_visible_models_and_reasoning_efforts() -> Result<()> {
    let server = start_mock_server().await;
    let mut visible_model = test_remote_model("visible-model", ModelVisibility::List, 1);
    visible_model.display_name = "Visible Model".to_string();
    visible_model.description = Some("Fast and capable".to_string());
    visible_model.default_reasoning_level = Some(ReasoningEffort::Medium);
    visible_model.supported_reasoning_levels = vec![
        ReasoningEffortPreset {
            effort: ReasoningEffort::Low,
            description: "Quick scan".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffort::High,
            description: "Deep dive".to_string(),
        },
    ];
    let mut hidden_model = test_remote_model("hidden-model", ModelVisibility::Hide, 1);
    hidden_model.display_name = "Hidden Model".to_string();
    hidden_model.description = Some("Should not be shown".to_string());
    hidden_model.default_reasoning_level = Some(ReasoningEffort::Low);
    hidden_model.supported_reasoning_levels = vec![ReasoningEffortPreset {
        effort: ReasoningEffort::Low,
        description: "Not visible".to_string(),
    }];
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![visible_model, hidden_model],
        },
    )
    .await;
    let resp_mock = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp1"), ev_completed("resp1")]),
    )
    .await;

    let mut builder = test_chaos()
        .with_auth(ChaosAuth::create_dummy_chatgpt_auth_for_testing())
        .with_model("visible-model");
    let test = builder.build(&server).await?;
    wait_for_model_available(&test.process_table.get_models_manager(), "visible-model").await;

    test.submit_turn("hello").await?;

    let body = resp_mock.single_request().body_json();
    let description =
        spawn_agent_description(&body).expect("spawn_agent description should be present");

    assert!(
        description.contains("Visible Model"),
        "expected visible model label in spawn_agent description: {description:?}"
    );
    assert!(
        description.contains("visible-model"),
        "expected visible model slug in spawn_agent description: {description:?}"
    );
    assert!(
        description.contains("Default reasoning effort"),
        "expected default reasoning effort in spawn_agent description: {description:?}"
    );
    assert!(
        description.contains("Quick scan") && description.contains("Deep dive"),
        "expected reasoning options in spawn_agent description: {description:?}"
    );
    assert!(
        !description.contains("Hidden Model"),
        "hidden picker model should be omitted from spawn_agent description: {description:?}"
    );
    assert!(
        description.contains("Only use `spawn_agent`"),
        "expected spawn_agent safety guidance in spawn_agent description: {description:?}"
    );

    Ok(())
}
