use chaos_ipc::protocol::{EventMsg, Op};
use chaos_ipc::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::{StreamingSseChunk, start_streaming_sse_server};
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event_with_timeout;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_warnings_are_sent_once_per_request_only_when_enabled() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    for enabled in [true, false] {
        let (server, _) = start_streaming_sse_server(
            ["first", "second"]
                .into_iter()
                .map(|id| {
                    vec![StreamingSseChunk {
                        gate: None,
                        body: responses::sse_completed(id),
                    }]
                })
                .collect(),
        )
        .await;
        let test = test_chaos()
            .with_config(move |config| {
                config.machine_warnings.enabled = enabled;
                // Exercise the real collector without filling disks or draining batteries.
                config.machine_warnings.disk_free_percent = 100;
                config.machine_warnings.probe_timeout_ms = 60_000;
            })
            .build_with_streaming_server(&server)
            .await?;
        for text in ["first turn", "second turn"] {
            test.process
                .submit(Op::UserInput {
                    items: vec![UserInput::Text {
                        text: text.into(),
                        text_elements: vec![],
                    }],
                    final_output_json_schema: None,
                })
                .await?;
            wait_for_event_with_timeout(
                &test.process,
                |event| matches!(event, EventMsg::TurnComplete(_)),
                Duration::from_secs(90),
            )
            .await;
        }
        let requests = server.requests().await;
        assert_eq!(requests.len(), 2);
        for request in requests {
            let body: serde_json::Value = serde_json::from_slice(&request)?;
            let warnings: Vec<_> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["role"] == "developer")
                .flat_map(|item| item["content"].as_array().into_iter().flatten())
                .filter_map(|content| content["text"].as_str())
                .filter(|text| text.contains("Machine warning (harness host)"))
                .collect();
            assert_eq!(warnings.len(), usize::from(enabled));
            if enabled {
                assert!(warnings[0].contains("minimal checkpoint"));
                assert!(warnings[0].contains("Tell the operator"));
            }
        }
        server.shutdown().await;
    }
    Ok(())
}
