use anyhow::Result;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_reasoning_item;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

async fn submit_turn(test: &TestChaos, text: &str) -> Result<Vec<String>> {
    test.process
        .submit(
            test.process
                .user_turn(
                    vec![UserInput::Text {
                        text: text.into(),
                        text_elements: Vec::new(),
                    }],
                    None,
                )
                .await,
        )
        .await?;

    let mut errors = Vec::new();
    loop {
        match wait_for_event(&test.process, |_| true).await {
            EventMsg::Error(err) => errors.push(err.message),
            EventMsg::TurnComplete(_) => return Ok(errors),
            _ => {}
        }
    }
}

fn reasoning_items(input: &[Value]) -> usize {
    input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_encrypted_reasoning_is_dropped_and_turn_retried() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_reasoning_item("rs-1", &["thinking"], &[]),
                ev_assistant_message("msg-1", "first"),
                ev_completed("resp-1"),
            ]),
            sse(vec![json!({
                "type": "error",
                "error": {
                    "message": "The encrypted content gAAA...tWc= could not be verified.",
                    "type": "invalid_request_error",
                    "param": "input",
                    "code": "invalid_encrypted_content"
                }
            })]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-2", "second"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let test = test_chaos().build(&server).await?;

    assert_eq!(submit_turn(&test, "one").await?, Vec::<String>::new());
    assert_eq!(submit_turn(&test, "two").await?, Vec::<String>::new());

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(reasoning_items(&requests[1].input()), 1);
    assert_eq!(reasoning_items(&requests[2].input()), 0);

    Ok(())
}
