#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::Settings;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::request_user_input::RequestUserInputAnswer;
use chaos_ipc::request_user_input::RequestUserInputResponse;
use chaos_ipc::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn call_output(req: &ResponsesRequest, call_id: &str) -> String {
    let raw = req.function_call_output(call_id);
    assert_eq!(
        raw.get("call_id").and_then(Value::as_str),
        Some(call_id),
        "mismatched call_id in function_call_output"
    );
    let (content_opt, _success) = match req.function_call_output_content_and_success(call_id) {
        Some(values) => values,
        None => panic!("function_call_output present"),
    };
    match content_opt {
        Some(content) => content,
        None => panic!("function_call_output content present"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_user_input_round_trip_resolves_pending() -> anyhow::Result<()> {
    request_user_input_round_trip_for_mode(ModeKind::Plan, false).await
}

/// Real waiting owner with deterministic model responses. An external phone
/// or the LAN test client must deliver the answer through the resident.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires dedicated Postgres, CHAOS_MOBILE_OWNER=1 and a paired client"]
async fn mobile_owner_round_trip() -> anyhow::Result<()> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    anyhow::ensure!(std::env::var("CHAOS_MOBILE_OWNER").as_deref() == Ok("1"));
    let pool = sqlx::PgPool::connect(&url).await?;
    chaos_vfs::set_root(chaos_vfs::mount(
        chaos_vfs::MountConfig::postgres_url(url),
        chaos_vfs::ChaosVfs::from_postgres_pool(pool),
    ));
    request_user_input_round_trip_for_mode(ModeKind::Plan, true).await
}

async fn request_user_input_round_trip_for_mode(
    mode: ModeKind,
    mobile: bool,
) -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_chaos();
    #[allow(clippy::expect_used)]
    let TestChaos {
        process: chaos,
        cwd,
        session_configured,
        home: _home,
        ..
    } = builder.build(&server).await?;

    let call_id = "user-input-call";
    let request_args = json!({
        "questions": [{
            "id": "confirm_path",
            "header": "Confirm",
            "question": "Proceed with the plan?",
            "options": [{
                "label": "Yes (Recommended)",
                "description": "Continue the current plan."
            }, {
                "label": "No",
                "description": "Stop and revisit the approach."
            }]
        }]
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "request_user_input", &request_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "thanks"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please confirm".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            vfs_policy: chaos_ipc::permissions::VfsPolicy::unrestricted(),
            socket_policy: chaos_ipc::permissions::SocketPolicy::Enabled,
            model: session_model,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: Some(CollaborationMode {
                mode,
                settings: Settings {
                    model: session_configured.model.clone(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            personality: None,
        })
        .await?;

    let request = wait_for_event_match(&chaos, |event| match event {
        EventMsg::RequestUserInput(request) => Some(request.clone()),
        _ => None,
    })
    .await;
    assert_eq!(request.call_id, call_id);
    assert_eq!(request.questions.len(), 1);
    assert_eq!(request.questions[0].is_other, true);

    let expected = if mobile {
        std::env::var("RELAY_TEST_REPLY").unwrap_or_else(|_| "Yes (Recommended)".to_string())
    } else {
        "yes".to_string()
    };
    let mut answers = HashMap::new();
    answers.insert(
        "confirm_path".to_string(),
        RequestUserInputAnswer {
            answers: vec![expected.clone()],
        },
    );
    let response = RequestUserInputResponse { answers };
    if !mobile {
        chaos
            .submit(Op::UserInputAnswer {
                id: request.turn_id.clone(),
                call_id: Some(request.call_id.clone()),
                response,
            })
            .await?;
    }

    if mobile {
        tokio::time::timeout(std::time::Duration::from_secs(300), async {
            let mut confirmed = false;
            loop {
                match chaos.next_event().await?.msg {
                    EventMsg::MobileQuestionAnswered { prompt, answer } => {
                        assert!(!confirmed, "duplicate phone confirmation");
                        assert_eq!(prompt.call_id, request.call_id);
                        assert_eq!(prompt.turn_id, request.turn_id);
                        assert_eq!(
                            answer.answers["confirm_path"].answers,
                            vec![expected.clone()]
                        );
                        confirmed = true;
                    }
                    EventMsg::TurnComplete(_) => break,
                    _ => {}
                }
            }
            assert!(
                confirmed,
                "owner completed without confirming a phone answer"
            );
            anyhow::Ok(())
        })
        .await??;
    } else {
        wait_for_event(&chaos, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    }

    let req = second_mock.single_request();
    let output_text = call_output(&req, call_id);
    let output_json: Value = serde_json::from_str(&output_text)?;
    assert_eq!(
        output_json,
        json!({
            "answers": {
                "confirm_path": { "answers": [expected] }
            }
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_user_input_round_trip_in_default_mode() -> anyhow::Result<()> {
    request_user_input_round_trip_for_mode(ModeKind::Default, false).await
}
