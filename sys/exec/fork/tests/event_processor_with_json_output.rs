use chaos_fork::event_processor_with_jsonl_output::EventProcessorWithJsonOutput;
use chaos_fork::exec_events::AgentMessageItem;
use chaos_fork::exec_events::CollabAgentState;
use chaos_fork::exec_events::CollabAgentStatus;
use chaos_fork::exec_events::CollabTool;
use chaos_fork::exec_events::CollabToolCallItem;
use chaos_fork::exec_events::CollabToolCallStatus;
use chaos_fork::exec_events::CommandExecutionItem;
use chaos_fork::exec_events::CommandExecutionStatus;
use chaos_fork::exec_events::ErrorItem;
use chaos_fork::exec_events::ItemCompletedEvent;
use chaos_fork::exec_events::ItemStartedEvent;
use chaos_fork::exec_events::ItemUpdatedEvent;
use chaos_fork::exec_events::McpToolCallItem;
use chaos_fork::exec_events::McpToolCallItemError;
use chaos_fork::exec_events::McpToolCallItemResult;
use chaos_fork::exec_events::McpToolCallStatus;
use chaos_fork::exec_events::PatchApplyStatus;
use chaos_fork::exec_events::PatchChangeKind;
use chaos_fork::exec_events::ProcessErrorEvent;
use chaos_fork::exec_events::ProcessEvent;
use chaos_fork::exec_events::ProcessItem;
use chaos_fork::exec_events::ProcessItemDetails;
use chaos_fork::exec_events::ProcessStartedEvent;
use chaos_fork::exec_events::ReasoningItem;
use chaos_fork::exec_events::TodoItem as ExecTodoItem;
use chaos_fork::exec_events::TodoListItem as ExecTodoListItem;
use chaos_fork::exec_events::TurnCompletedEvent;
use chaos_fork::exec_events::TurnFailedEvent;
use chaos_fork::exec_events::TurnStartedEvent;
use chaos_fork::exec_events::Usage;
use chaos_fork::exec_events::WebSearchItem;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::mcp::CallToolResult;
use chaos_ipc::models::WebSearchAction;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::plan_tool::PlanItemArg;
use chaos_ipc::plan_tool::StepStatus;
use chaos_ipc::plan_tool::UpdatePlanArgs;
use chaos_ipc::protocol::AgentMessageEvent;
use chaos_ipc::protocol::AgentReasoningEvent;
use chaos_ipc::protocol::AgentStatus;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::CollabAgentSpawnBeginEvent;
use chaos_ipc::protocol::CollabAgentSpawnEndEvent;
use chaos_ipc::protocol::CollabWaitingEndEvent;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecCommandBeginEvent;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandOutputDeltaEvent;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::protocol::ExecCommandStatus as CoreExecCommandStatus;
use chaos_ipc::protocol::ExecOutputStream;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::McpInvocation;
use chaos_ipc::protocol::McpToolCallBeginEvent;
use chaos_ipc::protocol::McpToolCallEndEvent;
use chaos_ipc::protocol::PatchApplyBeginEvent;
use chaos_ipc::protocol::PatchApplyEndEvent;
use chaos_ipc::protocol::PatchApplyStatus as CorePatchApplyStatus;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_ipc::protocol::WarningEvent;
use chaos_ipc::protocol::WebSearchBeginEvent;
use chaos_ipc::protocol::WebSearchEndEvent;
use mcp_guest::ContentBlock;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

fn event(id: &str, msg: EventMsg) -> Event {
    Event {
        id: id.to_string(),
        msg,
    }
}

fn assert_single_item_started(
    events: Vec<ProcessEvent>,
    expected_id: Option<&str>,
    expected_details: ProcessItemDetails,
) -> String {
    assert_eq!(events.len(), 1, "expected exactly one event: {events:?}");
    let ProcessEvent::ItemStarted(ItemStartedEvent { item }) = &events[0] else {
        panic!("expected ItemStarted");
    };
    if let Some(expected_id) = expected_id {
        assert_eq!(item.id, expected_id);
    } else {
        assert!(item.id.starts_with("item_"));
    }
    assert_eq!(item.details, expected_details);
    item.id.clone()
}

fn assert_single_item_completed(
    events: Vec<ProcessEvent>,
    expected_id: &str,
    expected_details: ProcessItemDetails,
) {
    assert_eq!(events.len(), 1, "expected exactly one event: {events:?}");
    let ProcessEvent::ItemCompleted(ItemCompletedEvent { item }) = &events[0] else {
        panic!("expected ItemCompleted");
    };
    assert_eq!(item.id, expected_id);
    assert_eq!(item.details, expected_details);
}

fn assert_begin_end_lifecycle(
    ep: &mut EventProcessorWithJsonOutput,
    begin: &Event,
    expected_started_id: Option<&str>,
    expected_started_details: ProcessItemDetails,
    end: &Event,
    expected_completed_details: ProcessItemDetails,
) {
    let started_id = assert_single_item_started(
        ep.collect_process_events(begin),
        expected_started_id,
        expected_started_details,
    );
    assert_single_item_completed(
        ep.collect_process_events(end),
        &started_id,
        expected_completed_details,
    );
}

fn web_search_item(
    id: &str,
    query: impl Into<String>,
    action: WebSearchAction,
) -> ProcessItemDetails {
    ProcessItemDetails::WebSearch(WebSearchItem {
        id: id.to_string(),
        query: query.into(),
        action,
    })
}

fn mcp_tool_call_item(
    server: &str,
    tool: &str,
    arguments: serde_json::Value,
    result: Option<McpToolCallItemResult>,
    error: Option<McpToolCallItemError>,
    status: McpToolCallStatus,
) -> ProcessItemDetails {
    ProcessItemDetails::McpToolCall(McpToolCallItem {
        server: server.to_string(),
        tool: tool.to_string(),
        arguments,
        result,
        error,
        status,
    })
}

fn collab_tool_call_item(
    tool: CollabTool,
    sender_process_id: &ProcessId,
    receiver_process_ids: Vec<String>,
    prompt: Option<String>,
    agents_states: std::collections::HashMap<String, CollabAgentState>,
    status: CollabToolCallStatus,
) -> ProcessItemDetails {
    ProcessItemDetails::CollabToolCall(CollabToolCallItem {
        tool,
        sender_process_id: sender_process_id.to_string(),
        receiver_process_ids,
        prompt,
        agents_states,
        status,
    })
}

fn command_execution_item(
    command: &str,
    aggregated_output: impl Into<String>,
    exit_code: Option<i32>,
    status: CommandExecutionStatus,
) -> ProcessItemDetails {
    ProcessItemDetails::CommandExecution(CommandExecutionItem {
        command: command.to_string(),
        aggregated_output: aggregated_output.into(),
        exit_code,
        status,
    })
}

fn file_changes<const N: usize>(
    entries: [(&str, FileChange); N],
) -> std::collections::HashMap<PathBuf, FileChange> {
    entries
        .into_iter()
        .map(|(path, change)| (PathBuf::from(path), change))
        .collect()
}

fn assert_patch_apply_completed(
    events: Vec<ProcessEvent>,
    expected_id: &str,
    expected_status: PatchApplyStatus,
    mut expected_changes: Vec<(String, PatchChangeKind)>,
) {
    assert_eq!(events.len(), 1, "expected exactly one event: {events:?}");
    let ProcessEvent::ItemCompleted(ItemCompletedEvent { item }) = &events[0] else {
        panic!("expected ItemCompleted");
    };
    assert_eq!(item.id, expected_id);

    let ProcessItemDetails::FileChange(file_update) = &item.details else {
        panic!("unexpected details: {:?}", item.details);
    };
    assert_eq!(file_update.status, expected_status);

    let mut actual: Vec<(String, PatchChangeKind)> = file_update
        .changes
        .iter()
        .map(|change| (change.path.clone(), change.kind.clone()))
        .collect();
    actual.sort_by(|a, b| a.0.cmp(&b.0));
    expected_changes.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(actual, expected_changes);
}

#[test]
fn session_configured_produces_process_started_event() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let session_id =
        chaos_ipc::ProcessId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
    let ev = event(
        "e1",
        EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id,
            forked_from_id: None,
            process_name: None,
            model: "chaos-mini-latest".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::Headless,
            approvals_reviewer: chaos_ipc::config_types::ApprovalsReviewer::User,
            vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
            socket_policy: chaos_ipc::protocol::SocketPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }),
    );
    let out = ep.collect_process_events(&ev);
    assert_eq!(
        out,
        vec![ProcessEvent::ProcessStarted(ProcessStartedEvent {
            process_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
        })]
    );
}

#[test]
fn task_started_produces_turn_started_event() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let out = ep.collect_process_events(&event(
        "t1",
        EventMsg::TurnStarted(chaos_ipc::protocol::TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: Some(32_000),
            collaboration_mode_kind: ModeKind::Default,
        }),
    ));

    assert_eq!(out, vec![ProcessEvent::TurnStarted(TurnStartedEvent {})]);
}

#[test]
fn web_search_end_emits_item_completed() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let query = "rust async await".to_string();
    let action = WebSearchAction::Search {
        query: Some(query.clone()),
        queries: None,
    };
    let out = ep.collect_process_events(&event(
        "w1",
        EventMsg::WebSearchEnd(WebSearchEndEvent {
            call_id: "call-123".to_string(),
            query: query.clone(),
            action: action.clone(),
        }),
    ));

    assert_single_item_completed(out, "item_0", web_search_item("call-123", query, action));
}

#[test]
fn web_search_begin_emits_item_started() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let out = ep.collect_process_events(&event(
        "w0",
        EventMsg::WebSearchBegin(WebSearchBeginEvent {
            call_id: "call-0".to_string(),
        }),
    ));

    assert_single_item_started(
        out,
        None,
        web_search_item("call-0", String::new(), WebSearchAction::Other),
    );
}

#[test]
fn web_search_begin_then_end_reuses_item_id() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let action = WebSearchAction::Search {
        query: Some("rust async await".to_string()),
        queries: None,
    };
    assert_begin_end_lifecycle(
        &mut ep,
        &event(
            "w0",
            EventMsg::WebSearchBegin(WebSearchBeginEvent {
                call_id: "call-1".to_string(),
            }),
        ),
        Some("item_0"),
        web_search_item("call-1", String::new(), WebSearchAction::Other),
        &event(
            "w1",
            EventMsg::WebSearchEnd(WebSearchEndEvent {
                call_id: "call-1".to_string(),
                query: "rust async await".to_string(),
                action: action.clone(),
            }),
        ),
        web_search_item("call-1", "rust async await", action),
    );
}

#[test]
fn plan_update_emits_todo_list_started_updated_and_completed() {
    let mut ep = EventProcessorWithJsonOutput::new(None);

    // First plan update => item.started (todo_list)
    let first = event(
        "p1",
        EventMsg::PlanUpdate(UpdatePlanArgs {
            explanation: None,
            plan: vec![
                PlanItemArg {
                    step: "step one".to_string(),
                    status: StepStatus::Pending,
                },
                PlanItemArg {
                    step: "step two".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
        }),
    );
    let out_first = ep.collect_process_events(&first);
    assert_eq!(
        out_first,
        vec![ProcessEvent::ItemStarted(ItemStartedEvent {
            item: ProcessItem {
                id: "item_0".to_string(),
                details: ProcessItemDetails::TodoList(ExecTodoListItem {
                    items: vec![
                        ExecTodoItem {
                            text: "step one".to_string(),
                            completed: false
                        },
                        ExecTodoItem {
                            text: "step two".to_string(),
                            completed: false
                        },
                    ],
                }),
            },
        })]
    );

    // Second plan update in same turn => item.updated (same id)
    let second = event(
        "p2",
        EventMsg::PlanUpdate(UpdatePlanArgs {
            explanation: None,
            plan: vec![
                PlanItemArg {
                    step: "step one".to_string(),
                    status: StepStatus::Completed,
                },
                PlanItemArg {
                    step: "step two".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
        }),
    );
    let out_second = ep.collect_process_events(&second);
    assert_eq!(
        out_second,
        vec![ProcessEvent::ItemUpdated(ItemUpdatedEvent {
            item: ProcessItem {
                id: "item_0".to_string(),
                details: ProcessItemDetails::TodoList(ExecTodoListItem {
                    items: vec![
                        ExecTodoItem {
                            text: "step one".to_string(),
                            completed: true
                        },
                        ExecTodoItem {
                            text: "step two".to_string(),
                            completed: false
                        },
                    ],
                }),
            },
        })]
    );

    // Task completes => item.completed (same id, latest state)
    let complete = event(
        "p3",
        EventMsg::TurnComplete(chaos_ipc::protocol::TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        }),
    );
    let out_complete = ep.collect_process_events(&complete);
    assert_eq!(
        out_complete,
        vec![
            ProcessEvent::ItemCompleted(ItemCompletedEvent {
                item: ProcessItem {
                    id: "item_0".to_string(),
                    details: ProcessItemDetails::TodoList(ExecTodoListItem {
                        items: vec![
                            ExecTodoItem {
                                text: "step one".to_string(),
                                completed: true
                            },
                            ExecTodoItem {
                                text: "step two".to_string(),
                                completed: false
                            },
                        ],
                    }),
                },
            }),
            ProcessEvent::TurnCompleted(TurnCompletedEvent {
                usage: Usage::default(),
            }),
        ]
    );
}

#[test]
fn mcp_tool_call_begin_and_end_emit_item_events() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let arguments = json!({ "key": "value" });
    let invocation = McpInvocation {
        server: "server_a".to_string(),
        tool: "tool_x".to_string(),
        arguments: Some(arguments.clone()),
    };

    assert_begin_end_lifecycle(
        &mut ep,
        &event(
            "m1",
            EventMsg::McpToolCallBegin(McpToolCallBeginEvent {
                call_id: "call-1".to_string(),
                invocation: invocation.clone(),
            }),
        ),
        Some("item_0"),
        mcp_tool_call_item(
            "server_a",
            "tool_x",
            arguments.clone(),
            None,
            None,
            McpToolCallStatus::InProgress,
        ),
        &event(
            "m2",
            EventMsg::McpToolCallEnd(McpToolCallEndEvent {
                call_id: "call-1".to_string(),
                invocation,
                duration: Duration::from_secs(1),
                result: Ok(CallToolResult {
                    content: Vec::new(),
                    is_error: None,
                    structured_content: None,
                    meta: None,
                }),
            }),
        ),
        mcp_tool_call_item(
            "server_a",
            "tool_x",
            arguments,
            Some(McpToolCallItemResult {
                content: Vec::new(),
                structured_content: None,
            }),
            None,
            McpToolCallStatus::Completed,
        ),
    );
}

#[test]
fn mcp_tool_call_failure_sets_failed_status() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let arguments = json!({ "param": 42 });
    let invocation = McpInvocation {
        server: "server_b".to_string(),
        tool: "tool_y".to_string(),
        arguments: Some(arguments.clone()),
    };

    assert_begin_end_lifecycle(
        &mut ep,
        &event(
            "m3",
            EventMsg::McpToolCallBegin(McpToolCallBeginEvent {
                call_id: "call-2".to_string(),
                invocation: invocation.clone(),
            }),
        ),
        Some("item_0"),
        mcp_tool_call_item(
            "server_b",
            "tool_y",
            arguments.clone(),
            None,
            None,
            McpToolCallStatus::InProgress,
        ),
        &event(
            "m4",
            EventMsg::McpToolCallEnd(McpToolCallEndEvent {
                call_id: "call-2".to_string(),
                invocation,
                duration: Duration::from_millis(5),
                result: Err("tool exploded".to_string()),
            }),
        ),
        mcp_tool_call_item(
            "server_b",
            "tool_y",
            arguments,
            None,
            Some(McpToolCallItemError {
                message: "tool exploded".to_string(),
            }),
            McpToolCallStatus::Failed,
        ),
    );
}

#[test]
fn mcp_tool_call_defaults_arguments_and_preserves_structured_content() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let invocation = McpInvocation {
        server: "server_c".to_string(),
        tool: "tool_z".to_string(),
        arguments: None,
    };

    let content = vec![serde_json::to_value(ContentBlock::text("done")).unwrap()];
    let structured_content = Some(json!({ "status": "ok" }));

    assert_begin_end_lifecycle(
        &mut ep,
        &event(
            "m5",
            EventMsg::McpToolCallBegin(McpToolCallBeginEvent {
                call_id: "call-3".to_string(),
                invocation: invocation.clone(),
            }),
        ),
        Some("item_0"),
        mcp_tool_call_item(
            "server_c",
            "tool_z",
            serde_json::Value::Null,
            None,
            None,
            McpToolCallStatus::InProgress,
        ),
        &event(
            "m6",
            EventMsg::McpToolCallEnd(McpToolCallEndEvent {
                call_id: "call-3".to_string(),
                invocation,
                duration: Duration::from_millis(10),
                result: Ok(CallToolResult {
                    content: content.clone(),
                    is_error: None,
                    structured_content: structured_content.clone(),
                    meta: None,
                }),
            }),
        ),
        mcp_tool_call_item(
            "server_c",
            "tool_z",
            serde_json::Value::Null,
            Some(McpToolCallItemResult {
                content,
                structured_content,
            }),
            None,
            McpToolCallStatus::Completed,
        ),
    );
}

#[test]
fn collab_spawn_begin_and_end_emit_item_events() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let sender_process_id = ProcessId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
    let new_process_id = ProcessId::from_string("9e107d9d-372b-4b8c-a2a4-1d9bb3fce0c1").unwrap();
    let prompt = "draft a plan".to_string();

    assert_begin_end_lifecycle(
        &mut ep,
        &event(
            "c1",
            EventMsg::CollabAgentSpawnBegin(CollabAgentSpawnBeginEvent {
                call_id: "call-10".to_string(),
                sender_process_id,
                prompt: prompt.clone(),
                model: "gpt-5".to_string(),
                reasoning_effort: ReasoningEffortConfig::default(),
                catchphrase: None,
                missing_topics: Vec::new(),
            }),
        ),
        Some("item_0"),
        collab_tool_call_item(
            CollabTool::SpawnAgent,
            &sender_process_id,
            Vec::new(),
            Some(prompt.clone()),
            std::collections::HashMap::new(),
            CollabToolCallStatus::InProgress,
        ),
        &event(
            "c2",
            EventMsg::CollabAgentSpawnEnd(CollabAgentSpawnEndEvent {
                call_id: "call-10".to_string(),
                sender_process_id,
                new_process_id: Some(new_process_id),
                new_agent_nickname: None,
                new_agent_role: None,
                prompt: prompt.clone(),
                model: "gpt-5".to_string(),
                reasoning_effort: ReasoningEffortConfig::default(),
                status: AgentStatus::Running,
            }),
        ),
        collab_tool_call_item(
            CollabTool::SpawnAgent,
            &sender_process_id,
            vec![new_process_id.to_string()],
            Some(prompt),
            [(
                new_process_id.to_string(),
                CollabAgentState {
                    status: CollabAgentStatus::Running,
                    message: None,
                },
            )]
            .into_iter()
            .collect(),
            CollabToolCallStatus::Completed,
        ),
    );
}

#[test]
fn collab_wait_end_without_begin_synthesizes_failed_item() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let sender_process_id = ProcessId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
    let running_process_id =
        ProcessId::from_string("3f76d2a0-943e-4f43-8a38-b289c9c6c3d1").unwrap();
    let failed_process_id = ProcessId::from_string("c1dfd96e-1f0c-4f26-9b4f-1aa02c2d3c4d").unwrap();
    let mut receiver_process_ids = vec![
        running_process_id.to_string(),
        failed_process_id.to_string(),
    ];
    receiver_process_ids.sort();
    let mut statuses = std::collections::HashMap::new();
    statuses.insert(
        running_process_id,
        AgentStatus::Completed(Some("done".to_string())),
    );
    statuses.insert(failed_process_id, AgentStatus::Errored("boom".to_string()));

    let end = event(
        "c3",
        EventMsg::CollabWaitingEnd(CollabWaitingEndEvent {
            sender_process_id,
            call_id: "call-11".to_string(),
            agent_statuses: Vec::new(),
            statuses: statuses.clone(),
        }),
    );
    let events = ep.collect_process_events(&end);
    assert_single_item_completed(
        events,
        "item_0",
        collab_tool_call_item(
            CollabTool::Wait,
            &sender_process_id,
            receiver_process_ids,
            None,
            [
                (
                    running_process_id.to_string(),
                    CollabAgentState {
                        status: CollabAgentStatus::Completed,
                        message: Some("done".to_string()),
                    },
                ),
                (
                    failed_process_id.to_string(),
                    CollabAgentState {
                        status: CollabAgentStatus::Errored,
                        message: Some("boom".to_string()),
                    },
                ),
            ]
            .into_iter()
            .collect(),
            CollabToolCallStatus::Failed,
        ),
    );
}

#[test]
fn plan_update_after_complete_starts_new_todo_list_with_new_id() {
    let mut ep = EventProcessorWithJsonOutput::new(None);

    // First turn: start + complete
    let start = event(
        "t1",
        EventMsg::PlanUpdate(UpdatePlanArgs {
            explanation: None,
            plan: vec![PlanItemArg {
                step: "only".to_string(),
                status: StepStatus::Pending,
            }],
        }),
    );
    let _ = ep.collect_process_events(&start);
    let complete = event(
        "t2",
        EventMsg::TurnComplete(chaos_ipc::protocol::TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        }),
    );
    let _ = ep.collect_process_events(&complete);

    // Second turn: a new todo list should have a new id
    let start_again = event(
        "t3",
        EventMsg::PlanUpdate(UpdatePlanArgs {
            explanation: None,
            plan: vec![PlanItemArg {
                step: "again".to_string(),
                status: StepStatus::Pending,
            }],
        }),
    );
    let out = ep.collect_process_events(&start_again);

    match &out[0] {
        ProcessEvent::ItemStarted(ItemStartedEvent { item }) => {
            assert_eq!(&item.id, "item_1");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn agent_reasoning_produces_item_completed_reasoning() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let ev = event(
        "e1",
        EventMsg::AgentReasoning(AgentReasoningEvent {
            text: "thinking...".to_string(),
        }),
    );
    let out = ep.collect_process_events(&ev);
    assert_eq!(
        out,
        vec![ProcessEvent::ItemCompleted(ItemCompletedEvent {
            item: ProcessItem {
                id: "item_0".to_string(),
                details: ProcessItemDetails::Reasoning(ReasoningItem {
                    text: "thinking...".to_string(),
                }),
            },
        })]
    );
}

#[test]
fn agent_message_produces_item_completed_agent_message() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let ev = event(
        "e1",
        EventMsg::AgentMessage(AgentMessageEvent {
            message: "hello".to_string(),
            phase: None,
        }),
    );
    let out = ep.collect_process_events(&ev);
    assert_eq!(
        out,
        vec![ProcessEvent::ItemCompleted(ItemCompletedEvent {
            item: ProcessItem {
                id: "item_0".to_string(),
                details: ProcessItemDetails::AgentMessage(AgentMessageItem {
                    text: "hello".to_string(),
                }),
            },
        })]
    );
}

#[test]
fn error_event_produces_error() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let out = ep.collect_process_events(&event(
        "e1",
        EventMsg::Error(chaos_ipc::protocol::ErrorEvent {
            message: "boom".to_string(),
            chaos_error_info: Some(ChaosErrorInfo::Other),
        }),
    ));
    assert_eq!(
        out,
        vec![ProcessEvent::Error(ProcessErrorEvent {
            message: "boom".to_string(),
        })]
    );
}

#[test]
fn warning_event_produces_error_item() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let out = ep.collect_process_events(&event(
        "e1",
        EventMsg::Warning(WarningEvent {
            message: "Heads up: Long conversations and multiple compactions can cause the model to be less accurate. Start a new conversation when possible to keep conversations small and targeted.".to_string(),
        }),
    ));
    assert_eq!(
        out,
        vec![ProcessEvent::ItemCompleted(ItemCompletedEvent {
            item: ProcessItem {
                id: "item_0".to_string(),
                details: ProcessItemDetails::Error(ErrorItem {
                    message: "Heads up: Long conversations and multiple compactions can cause the model to be less accurate. Start a new conversation when possible to keep conversations small and targeted.".to_string(),
                }),
            },
        })]
    );
}

#[test]
fn stream_error_event_produces_error() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let out = ep.collect_process_events(&event(
        "e1",
        EventMsg::StreamError(chaos_ipc::protocol::StreamErrorEvent {
            message: "retrying".to_string(),
            chaos_error_info: Some(ChaosErrorInfo::Other),
            additional_details: None,
        }),
    ));
    assert_eq!(
        out,
        vec![ProcessEvent::Error(ProcessErrorEvent {
            message: "retrying".to_string(),
        })]
    );
}

#[test]
fn error_followed_by_task_complete_produces_turn_failed() {
    let mut ep = EventProcessorWithJsonOutput::new(None);

    let error_event = event(
        "e1",
        EventMsg::Error(ErrorEvent {
            message: "boom".to_string(),
            chaos_error_info: Some(ChaosErrorInfo::Other),
        }),
    );
    assert_eq!(
        ep.collect_process_events(&error_event),
        vec![ProcessEvent::Error(ProcessErrorEvent {
            message: "boom".to_string(),
        })]
    );

    let complete_event = event(
        "e2",
        EventMsg::TurnComplete(chaos_ipc::protocol::TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        }),
    );
    assert_eq!(
        ep.collect_process_events(&complete_event),
        vec![ProcessEvent::TurnFailed(TurnFailedEvent {
            error: ProcessErrorEvent {
                message: "boom".to_string(),
            },
        })]
    );
}

#[test]
fn exec_command_end_success_produces_completed_command_item() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let command = vec!["bash".to_string(), "-lc".to_string(), "echo hi".to_string()];
    let cwd = std::env::current_dir().unwrap();
    let parsed_cmd = Vec::new();

    assert_begin_end_lifecycle(
        &mut ep,
        &event(
            "c1",
            EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
                call_id: "1".to_string(),
                process_id: None,
                turn_id: "turn-1".to_string(),
                command: command.clone(),
                cwd: cwd.clone(),
                parsed_cmd: parsed_cmd.clone(),
                source: ExecCommandSource::Agent,
                interaction_input: None,
            }),
        ),
        Some("item_0"),
        command_execution_item(
            "bash -lc 'echo hi'",
            String::new(),
            None,
            CommandExecutionStatus::InProgress,
        ),
        &event(
            "c2",
            EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "1".to_string(),
                process_id: None,
                turn_id: "turn-1".to_string(),
                command,
                cwd,
                parsed_cmd,
                source: ExecCommandSource::Agent,
                interaction_input: None,
                stdout: String::new(),
                stderr: String::new(),
                aggregated_output: "hi\n".to_string(),
                exit_code: 0,
                duration: Duration::from_millis(5),
                formatted_output: String::new(),
                status: CoreExecCommandStatus::Completed,
            }),
        ),
        command_execution_item(
            "bash -lc 'echo hi'",
            "hi\n",
            Some(0),
            CommandExecutionStatus::Completed,
        ),
    );
}

#[test]
fn command_execution_output_delta_updates_item_progress() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let command = vec![
        "bash".to_string(),
        "-lc".to_string(),
        "echo delta".to_string(),
    ];
    let cwd = std::env::current_dir().unwrap();
    let parsed_cmd = Vec::new();

    let begin = event(
        "d1",
        EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
            call_id: "delta-1".to_string(),
            process_id: Some("42".to_string()),
            turn_id: "turn-1".to_string(),
            command: command.clone(),
            cwd: cwd.clone(),
            parsed_cmd: parsed_cmd.clone(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
        }),
    );
    assert_single_item_started(
        ep.collect_process_events(&begin),
        Some("item_0"),
        command_execution_item(
            "bash -lc 'echo delta'",
            String::new(),
            None,
            CommandExecutionStatus::InProgress,
        ),
    );

    let delta = event(
        "d2",
        EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
            call_id: "delta-1".to_string(),
            stream: ExecOutputStream::Stdout,
            chunk: b"partial output\n".to_vec(),
        }),
    );
    let out_delta = ep.collect_process_events(&delta);
    assert_eq!(out_delta, Vec::<ProcessEvent>::new());

    let end = event(
        "d3",
        EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "delta-1".to_string(),
            process_id: Some("42".to_string()),
            turn_id: "turn-1".to_string(),
            command,
            cwd,
            parsed_cmd,
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: String::new(),
            stderr: String::new(),
            aggregated_output: String::new(),
            exit_code: 0,
            duration: Duration::from_millis(3),
            formatted_output: String::new(),
            status: CoreExecCommandStatus::Completed,
        }),
    );
    assert_single_item_completed(
        ep.collect_process_events(&end),
        "item_0",
        command_execution_item(
            "bash -lc 'echo delta'",
            String::new(),
            Some(0),
            CommandExecutionStatus::Completed,
        ),
    );
}

#[test]
fn exec_command_end_failure_produces_failed_command_item() {
    let mut ep = EventProcessorWithJsonOutput::new(None);
    let command = vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()];
    let cwd = std::env::current_dir().unwrap();
    let parsed_cmd = Vec::new();

    assert_begin_end_lifecycle(
        &mut ep,
        &event(
            "c1",
            EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
                call_id: "2".to_string(),
                process_id: None,
                turn_id: "turn-1".to_string(),
                command: command.clone(),
                cwd: cwd.clone(),
                parsed_cmd: parsed_cmd.clone(),
                source: ExecCommandSource::Agent,
                interaction_input: None,
            }),
        ),
        Some("item_0"),
        command_execution_item(
            "sh -c 'exit 1'",
            String::new(),
            None,
            CommandExecutionStatus::InProgress,
        ),
        &event(
            "c2",
            EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "2".to_string(),
                process_id: None,
                turn_id: "turn-1".to_string(),
                command,
                cwd,
                parsed_cmd,
                source: ExecCommandSource::Agent,
                interaction_input: None,
                stdout: String::new(),
                stderr: String::new(),
                aggregated_output: String::new(),
                exit_code: 1,
                duration: Duration::from_millis(2),
                formatted_output: String::new(),
                status: CoreExecCommandStatus::Failed,
            }),
        ),
        command_execution_item(
            "sh -c 'exit 1'",
            String::new(),
            Some(1),
            CommandExecutionStatus::Failed,
        ),
    );
}

#[test]
fn exec_command_end_without_begin_is_ignored() {
    let mut ep = EventProcessorWithJsonOutput::new(None);

    // End event arrives without a prior Begin; should produce no thread events.
    let end_only = event(
        "c1",
        EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "no-begin".to_string(),
            process_id: None,
            turn_id: "turn-1".to_string(),
            command: Vec::new(),
            cwd: PathBuf::from("."),
            parsed_cmd: Vec::new(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: String::new(),
            stderr: String::new(),
            aggregated_output: String::new(),
            exit_code: 0,
            duration: Duration::from_millis(1),
            formatted_output: String::new(),
            status: CoreExecCommandStatus::Completed,
        }),
    );
    let out = ep.collect_process_events(&end_only);
    assert!(out.is_empty());
}

#[test]
fn patch_apply_success_produces_item_completed_patchapply() {
    let mut ep = EventProcessorWithJsonOutput::new(None);

    let changes = file_changes([
        (
            "a/added.txt",
            FileChange::Add {
                content: "+hello".to_string(),
            },
        ),
        (
            "b/deleted.txt",
            FileChange::Delete {
                content: "-goodbye".to_string(),
            },
        ),
        (
            "c/modified.txt",
            FileChange::Update {
                unified_diff: "--- c/modified.txt\n+++ c/modified.txt\n@@\n-old\n+new\n"
                    .to_string(),
                move_path: Some(PathBuf::from("c/renamed.txt")),
            },
        ),
    ]);

    assert!(
        ep.collect_process_events(&event(
            "p1",
            EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
                call_id: "call-1".to_string(),
                turn_id: "turn-1".to_string(),
                auto_approved: true,
                changes: changes.clone(),
            }),
        ))
        .is_empty()
    );

    assert_patch_apply_completed(
        ep.collect_process_events(&event(
            "p2",
            EventMsg::PatchApplyEnd(PatchApplyEndEvent {
                call_id: "call-1".to_string(),
                turn_id: "turn-1".to_string(),
                stdout: "applied 3 changes".to_string(),
                stderr: String::new(),
                success: true,
                changes,
                status: CorePatchApplyStatus::Completed,
            }),
        )),
        "item_0",
        PatchApplyStatus::Completed,
        vec![
            ("a/added.txt".to_string(), PatchChangeKind::Add),
            ("b/deleted.txt".to_string(), PatchChangeKind::Delete),
            ("c/modified.txt".to_string(), PatchChangeKind::Update),
        ],
    );
}

#[test]
fn patch_apply_failure_produces_item_completed_patchapply_failed() {
    let mut ep = EventProcessorWithJsonOutput::new(None);

    let changes = file_changes([(
        "file.txt",
        FileChange::Update {
            unified_diff: "--- file.txt\n+++ file.txt\n@@\n-old\n+new\n".to_string(),
            move_path: None,
        },
    )]);

    assert!(
        ep.collect_process_events(&event(
            "p1",
            EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
                call_id: "call-2".to_string(),
                turn_id: "turn-2".to_string(),
                auto_approved: false,
                changes: changes.clone(),
            }),
        ))
        .is_empty()
    );

    assert_patch_apply_completed(
        ep.collect_process_events(&event(
            "p2",
            EventMsg::PatchApplyEnd(PatchApplyEndEvent {
                call_id: "call-2".to_string(),
                turn_id: "turn-2".to_string(),
                stdout: String::new(),
                stderr: "failed to apply".to_string(),
                success: false,
                changes,
                status: CorePatchApplyStatus::Failed,
            }),
        )),
        "item_0",
        PatchApplyStatus::Failed,
        vec![("file.txt".to_string(), PatchChangeKind::Update)],
    );
}

#[test]
fn task_complete_produces_turn_completed_with_usage() {
    let mut ep = EventProcessorWithJsonOutput::new(None);

    // First, feed a TokenCount event with known totals.
    let usage = chaos_ipc::protocol::TokenUsage {
        input_tokens: 1200,
        cached_input_tokens: 200,
        output_tokens: 345,
        reasoning_output_tokens: 0,
        total_tokens: 0,
    };
    let info = chaos_ipc::protocol::TokenUsageInfo {
        total_token_usage: usage.clone(),
        last_token_usage: usage,
        model_context_window: None,
    };
    let token_count_event = event(
        "e1",
        EventMsg::TokenCount(chaos_ipc::protocol::TokenCountEvent {
            info: Some(info),
            rate_limits: None,
        }),
    );
    assert!(ep.collect_process_events(&token_count_event).is_empty());

    // Then TurnComplete should produce turn.completed with the captured usage.
    let complete_event = event(
        "e2",
        EventMsg::TurnComplete(chaos_ipc::protocol::TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: Some("done".to_string()),
        }),
    );
    let out = ep.collect_process_events(&complete_event);
    assert_eq!(
        out,
        vec![ProcessEvent::TurnCompleted(TurnCompletedEvent {
            usage: Usage {
                input_tokens: 1200,
                cached_input_tokens: 200,
                output_tokens: 345,
            },
        })]
    );
}
