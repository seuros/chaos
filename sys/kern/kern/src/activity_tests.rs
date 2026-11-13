use super::*;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandSource;
use pretty_assertions::assert_eq;
use std::time::Duration;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(ToString::to_string).collect()
}

#[test]
fn a_finished_command_carries_no_arguments_or_output() {
    let msg = EventMsg::ExecCommandEnd(ExecCommandEndEvent {
        call_id: "call-1".to_string(),
        process_id: None,
        turn_id: "turn-1".to_string(),
        command: argv(&["bash", "-lc", "curl -H 'token: hunter2' https://x"]),
        cwd: "/home/someone/private".into(),
        parsed_cmd: Vec::new(),
        source: ExecCommandSource::Agent,
        interaction_input: Some("hunter2".to_string()),
        stdout: "hunter2".to_string(),
        stderr: "hunter2".to_string(),
        aggregated_output: "hunter2".to_string(),
        exit_code: 1,
        duration: Duration::from_millis(42),
        formatted_output: "hunter2".to_string(),
        status: ExecCommandStatus::Failed,
    });
    let [activity] = Activity::from_event(&msg).try_into().unwrap();
    assert_eq!(
        activity,
        Activity::CallEnded {
            call_id: "call-1".to_string(),
            station: Station::Network,
            label: "curl".to_string(),
            outcome: CallOutcome::Failed,
            duration_ms: Some(42),
        }
    );
    let json = serde_json::to_string(&activity).unwrap();
    assert!(
        !json.contains("hunter2") && !json.contains("private"),
        "{json}"
    );
}

#[tokio::test]
async fn subscribers_receive_what_sessions_deliver() {
    let mut rx = subscribe();
    let (session, turn_context) = crate::chaos::make_session_and_context().await;
    session
        .send_event(
            &turn_context,
            EventMsg::Error(chaos_ipc::protocol::ErrorEvent {
                message: "provider said: hunter2".to_string(),
                chaos_error_info: None,
            }),
        )
        .await;
    // The bus is shared by every test in this binary; find ours.
    let ours = async {
        loop {
            let event = rx.recv().await.unwrap();
            if event.process_id == session.conversation_id {
                return event.activity;
            }
        }
    };
    let activity = tokio::time::timeout(Duration::from_secs(5), ours)
        .await
        .expect("the session's event never reached the bus");
    assert_eq!(activity, Activity::Failed);
}

#[tokio::test]
async fn an_answered_ask_is_resolved_under_the_id_it_was_raised_with() {
    let mut rx = subscribe();
    let (session, turn_context) = crate::chaos::make_session_and_context().await;
    *session.active_turn.lock().await = Some(crate::state::ActiveTurn::default());
    let decision = session
        .request_patch_approval(
            &turn_context,
            "patch-1".to_string(),
            std::collections::HashMap::new(),
            None,
            None,
        )
        .await;
    session
        .notify_patch_approval("patch-1", chaos_ipc::protocol::ReviewDecision::Approved)
        .await;
    assert_eq!(
        decision.await.unwrap(),
        chaos_ipc::protocol::ReviewDecision::Approved
    );
    let ours = async {
        let mut seen = Vec::new();
        while seen.len() < 2 {
            let event = rx.recv().await.unwrap();
            if event.process_id == session.conversation_id {
                seen.push(event.activity);
            }
        }
        seen
    };
    let seen = tokio::time::timeout(Duration::from_secs(5), ours)
        .await
        .expect("the ask and its answer never reached the bus");
    assert_eq!(
        seen,
        vec![
            Activity::NeedsYou {
                ask_id: "patch-1".to_string(),
                reason: Attention::PatchApproval,
            },
            Activity::Resolved {
                ask_id: "patch-1".to_string(),
            },
        ]
    );
}

#[tokio::test]
async fn model_requests_end_with_their_usage_or_as_failed() {
    let mut rx = subscribe();
    let process_id = ProcessId::new();
    ModelRequest::start(process_id, 1, "model-a").finish(Some(&chaos_ipc::protocol::TokenUsage {
        input_tokens: 100,
        cached_input_tokens: 60,
        cache_creation_input_tokens: 10,
        output_tokens: 7,
        reasoning_output_tokens: 3,
        ..Default::default()
    }));
    drop(ModelRequest::start(process_id, 2, "model-a"));
    let ours = async {
        let mut seen = Vec::new();
        while seen.len() < 4 {
            let event = rx.recv().await.unwrap();
            if event.process_id == process_id {
                seen.push(event.activity);
            }
        }
        seen
    };
    let seen = tokio::time::timeout(Duration::from_secs(5), ours)
        .await
        .expect("model requests never reached the bus");
    let ends: Vec<_> = seen
        .into_iter()
        .filter_map(|activity| match activity {
            Activity::ModelEnded {
                request,
                outcome,
                usage,
                ..
            } => Some((request, outcome, usage)),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends,
        vec![
            (
                1,
                CallOutcome::Ok,
                Some(ModelUsage {
                    input: 100,
                    cache_read: 60,
                    cache_write: 10,
                    output: 7,
                    reasoning: 3,
                })
            ),
            (2, CallOutcome::Failed, None),
        ]
    );
}
