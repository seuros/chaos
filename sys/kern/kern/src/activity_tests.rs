use super::*;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::protocol::ExecCommandStatus;
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
            check: None,
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
        cache_read_input_tokens: 60,
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

#[test]
fn a_piped_test_run_reports_failures_its_exit_status_hid() {
    let msg = EventMsg::ExecCommandEnd(ExecCommandEndEvent {
        call_id: "call-2".to_string(),
        process_id: None,
        turn_id: "turn-1".to_string(),
        command: argv(&["bash", "-lc", "cargo test 2>&1 | tail -n 3"]),
        cwd: "/home/someone/private".into(),
        parsed_cmd: Vec::new(),
        source: ExecCommandSource::Agent,
        interaction_input: None,
        stdout: String::new(),
        stderr: String::new(),
        aggregated_output: "thread 'secret_test' panicked: hunter2\n\
            test result: FAILED. 9 passed; 1 failed; 0 ignored; 0 measured"
            .to_string(),
        exit_code: 0,
        duration: Duration::from_millis(900),
        formatted_output: String::new(),
        status: ExecCommandStatus::Completed,
    });
    let [activity] = Activity::from_event(&msg).try_into().unwrap();
    let Activity::CallEnded { outcome, check, .. } = &activity else {
        panic!("{activity:?}");
    };
    assert_eq!(*outcome, CallOutcome::Ok);
    assert_eq!(
        *check,
        Some(Check {
            kind: CheckKind::Test,
            counts: Some(CheckCounts {
                passed: 9,
                failed: 1,
                skipped: 0,
            }),
        })
    );
    let json = serde_json::to_string(&activity).unwrap();
    assert!(
        !json.contains("hunter2") && !json.contains("secret_test"),
        "{json}"
    );
}

#[test]
fn internal_tools_are_filed_by_what_they_do_and_reported_once() {
    let call = |server: Option<&str>, tool: &str| {
        EventMsg::McpToolCallBegin(chaos_ipc::protocol::McpToolCallBeginEvent {
            call_id: "call-3".to_string(),
            invocation: chaos_ipc::protocol::McpInvocation {
                server: server.map(str::to_string),
                tool: tool.to_string(),
                arguments: None,
            },
        })
    };
    let station = |msg: &EventMsg| match Activity::from_event(msg).as_slice() {
        [Activity::CallStarted { station, .. }] => Some(*station),
        [] => None,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        station(&call(None, "save_recovery_checkpoint")),
        Some(Station::Kernel)
    );
    assert_eq!(station(&call(None, "read_file")), Some(Station::Files));
    assert_eq!(station(&call(None, "spawn_agent")), Some(Station::Agents));
    assert_eq!(
        station(&call(None, "exec_command")),
        None,
        "the exec events report it"
    );
    assert_eq!(
        station(&call(Some("docs"), "read_file")),
        Some(Station::Mcp)
    );
}

fn token_count(used: i64, percent: f64) -> EventMsg {
    let usage = chaos_ipc::protocol::TokenUsage {
        total_tokens: used,
        ..Default::default()
    };
    EventMsg::TokenCount(chaos_ipc::protocol::TokenCountEvent {
        info: Some(chaos_ipc::protocol::TokenUsageInfo {
            total_token_usage: usage.clone(),
            last_token_usage: usage,
            model_context_window: Some(1000),
        }),
        rate_limits: Some(chaos_ipc::protocol::RateLimitSnapshot {
            primary: Some(chaos_ipc::protocol::RateLimitWindow {
                used_percent: percent,
                window_minutes: Some(60),
                resets_at: None,
            }),
            limit_id: None,
            limit_name: None,
            secondary: None,
            credits: None,
            plan_type: None,
        }),
        provider_request_started: false,
    })
}

async fn ours(
    rx: &mut broadcast::Receiver<ActivityEvent>,
    process_id: ProcessId,
    n: usize,
) -> Vec<Activity> {
    let collect = async {
        let mut seen = Vec::new();
        while seen.len() < n {
            let event = rx.recv().await.unwrap();
            if event.process_id == process_id {
                seen.push(event.activity);
            }
        }
        seen
    };
    tokio::time::timeout(Duration::from_secs(5), collect)
        .await
        .expect("activity never reached the bus")
}

#[tokio::test]
async fn gauges_reach_the_bus_only_when_they_change() {
    let mut rx = subscribe();
    let process_id = ProcessId::new();
    publish(process_id, &token_count(10, 5.0));
    publish(process_id, &token_count(10, 5.0));
    publish(process_id, &token_count(20, 5.0));
    publish(
        process_id,
        &EventMsg::TurnComplete(chaos_ipc::protocol::TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        }),
    );
    publish(process_id, &token_count(20, 5.0));
    let context = |used_tokens| Activity::Context {
        used_tokens,
        window: Some(1000),
    };
    let limits = Activity::Limits {
        windows: vec![LimitWindow {
            used_percent: 5,
            window_minutes: Some(60),
            resets_at: None,
        }],
    };
    assert_eq!(
        ours(&mut rx, process_id, 6).await,
        vec![
            context(10),
            limits.clone(),
            context(20),
            Activity::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
            context(20),
            limits,
        ]
    );
}

#[tokio::test]
async fn a_machine_phase_names_its_causes() {
    let mut rx = subscribe();
    let process_id = ProcessId::new();
    machine(
        process_id,
        crate::machine_recovery::Phase::Warning,
        vec!["disk", "thermal"],
    );
    assert_eq!(
        ours(&mut rx, process_id, 1).await,
        vec![Activity::Machine {
            phase: MachinePhase::Warning,
            causes: vec![MachineCause::Disk, MachineCause::Thermal],
        }]
    );
}

#[test]
fn minions_report_their_state_and_their_parent_waiting_on_them() {
    let child = ProcessId::new();
    let status =
        EventMsg::CollabAgentStatusChanged(chaos_ipc::protocol::CollabAgentStatusChangedEvent {
            parent_process_id: ProcessId::new(),
            child_process_id: child,
            agent_nickname: Some("Ada".to_string()),
            agent_role: None,
            model: None,
            status: chaos_ipc::protocol::CollabAgentStatus::Errored,
        });
    assert_eq!(
        Activity::from_event(&status),
        vec![Activity::AgentState {
            child,
            state: AgentRun::Failed,
        }]
    );
    let waiting = EventMsg::CollabWaitingBegin(chaos_ipc::protocol::CollabWaitingBeginEvent {
        sender_process_id: ProcessId::new(),
        receiver_process_ids: vec![child],
        receiver_agents: Vec::new(),
        call_id: "wait-1".to_string(),
    });
    assert_eq!(
        Activity::from_event(&waiting),
        vec![Activity::WaitingOnAgents {
            children: vec![child],
        }]
    );
}

#[test]
fn a_clamped_cli_using_chaos_tools_is_chaos_work_not_an_mcp_server() {
    let bridged = |tool: &str| {
        EventMsg::McpToolCallBegin(chaos_ipc::protocol::McpToolCallBeginEvent {
            call_id: format!("{}1", crate::client::CLAMP_MCP_CALL_PREFIX),
            invocation: chaos_ipc::protocol::McpInvocation {
                server: Some("chaos".to_string()),
                tool: tool.to_string(),
                arguments: None,
            },
        })
    };
    let activity: [Activity; 1] = Activity::from_event(&bridged("read_file"))
        .try_into()
        .unwrap();
    let [Activity::CallStarted { station, label, .. }] = activity else {
        panic!("a bridged read must be reported: {activity:?}");
    };
    assert_eq!((station, label.as_str()), (Station::Files, "read_file"));
    assert_eq!(
        Activity::from_event(&bridged("exec_command")),
        Vec::new(),
        "the command reports itself through its exec events"
    );
}

#[tokio::test]
async fn a_request_reports_the_model_that_served_it() {
    let mut rx = subscribe();
    let process_id = ProcessId::new();
    let mut request = ModelRequest::start(process_id, 1, "configured-model");
    request.served_by("claude-served");
    request.finish(None);
    let collect = async {
        loop {
            let event = rx.recv().await.unwrap();
            if event.process_id == process_id
                && let Activity::ModelEnded { model, .. } = event.activity
            {
                return model;
            }
        }
    };
    let model = tokio::time::timeout(Duration::from_secs(5), collect)
        .await
        .expect("the request never ended on the bus");
    assert_eq!(model, "claude-served");
}
