//! Verifies that the agent retries when the SSE stream terminates before
//! delivering a `response.completed` event.

use chaos_ipc::protocol::EventMsg;
use chaos_ipc::user_input::UserInput;
use chaos_kern::ModelProviderInfo;
use chaos_kern::WireApi;
use chaos_which::find_resource;
use core_test_support::load_sse_fixture;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use serde_json::json;
use std::assert_matches;

fn sse_incomplete() -> String {
    let fixture = find_resource!("tests/fixtures/incomplete_sse.json")
        .unwrap_or_else(|err| panic!("failed to resolve incomplete_sse fixture: {err}"));
    load_sse_fixture(fixture)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_on_early_close() {
    skip_if_no_network!();

    let incomplete_sse = sse_incomplete();
    let completed_sse = responses::sse_completed("resp_ok");

    let (server, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: incomplete_sse,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: completed_sse,
        }],
    ])
    .await;

    // Configure retry behavior explicitly to avoid mutating process-wide
    // environment variables.

    let model_provider = ModelProviderInfo {
        name: "openai".into(),
        model_family: Default::default(),
        model_family_overrides: Default::default(),
        base_url: Some(format!("{}/v1", server.uri())),
        // Environment variable that should exist in the test environment.
        // ModelClient will return an error if the environment variable for the
        // provider is not set.
        env_key: Some("PATH".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        // exercise retry path: first attempt yields incomplete stream, so allow 1 retry
        request_max_retries: Some(0),
        stream_max_retries: Some(1),
        stream_idle_timeout_ms: Some(2000),
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
        egress: None,
    };

    let TestChaos {
        process: chaos,
        home: _home,
        cwd: _cwd,
        ..
    } = test_chaos()
        .with_config(move |config| {
            config.model_provider = model_provider;
        })
        .build_with_streaming_server(&server)
        .await
        .unwrap();

    chaos
        .submit(
            chaos
                .user_turn(
                    vec![UserInput::Text {
                        text: "hello".into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await
        .unwrap();

    // Wait until TurnComplete (should succeed after retry).
    wait_for_event(&chaos, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let requests = server.requests().await;
    assert_eq!(
        requests.len(),
        2,
        "expected retry after incomplete SSE stream"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_close_after_tool_dispatch_preserves_results_without_replaying() -> anyhow::Result<()>
{
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let test = test_chaos()
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
        })
        .build(&server)
        .await?;
    let input = test.cwd.path().join("sample-input.txt");
    std::fs::write(&input, "settled-tool-result")?;
    let call_id = "read-before-disconnect";
    let first = responses::sse(vec![
        responses::ev_response_created("incomplete-tool-response"),
        responses::ev_function_call(
            call_id,
            "read_file",
            &json!({"file_path": input, "limit": 1}).to_string(),
        ),
        // Intentionally omit response.completed after dispatching the tool.
    ]);
    let next = responses::sse(vec![
        responses::ev_assistant_message("next-message", "resumed"),
        responses::ev_completed("next-response"),
    ]);
    let mock = responses::mount_sse_sequence(&server, vec![first, next]).await;

    test.process
        .submit(
            test.process
                .user_turn(
                    vec![UserInput::Text {
                        text: "read the input".into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await?;
    let error = wait_for_event(&test.process, |event| matches!(event, EventMsg::Error(_))).await;
    assert_matches!(
        error,
        EventMsg::Error(error) if error.message.contains("automatic retry refused")
    );
    wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.requests().len(), 1, "failed sample must not replay");

    test.submit_turn("continue using the recorded result")
        .await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .function_call_output_text(call_id)
            .is_some_and(|output| output.contains("settled-tool-result")),
        "tool output must settle into history even when the stream fails"
    );
    Ok(())
}
