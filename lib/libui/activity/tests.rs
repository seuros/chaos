use super::*;
use chaos_ipc::protocol::*;

fn observe(tracker: &mut Tracker, id: ProcessId, msg: EventMsg, now: Instant) {
    tracker.observe(
        id,
        &Event {
            id: "turn".into(),
            msg,
        },
        now,
    );
}

fn start(tracker: &mut Tracker, id: ProcessId, now: Instant) {
    observe(
        tracker,
        id,
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn".into(),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        now,
    );
}

fn delta(turn: &str, text: &str) -> EventMsg {
    EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
        process_id: String::new(),
        turn_id: turn.into(),
        item_id: "message".into(),
        delta: text.into(),
    })
}

#[test]
fn estimates_and_empty_deltas_do_not_renew_observed_activity() {
    let mut tracker = Tracker::default();
    let id = ProcessId::new();
    let now = Instant::now();
    start(&mut tracker, id, now);
    let later = now + Duration::from_secs(45);
    observe(
        &mut tracker,
        id,
        EventMsg::TurnProgress(TurnProgressEvent {
            turn_id: "turn".into(),
            approx_reasoning_tokens: 999,
            approx_output_tokens: 0,
            approx_total_tokens: 999,
        }),
        later,
    );
    observe(&mut tracker, id, delta("turn", ""), later);
    let activity = tracker.get(id);
    assert_eq!(activity.last_activity, Some(now));
    assert_eq!(activity.last_runtime_event, Some(later));
    assert!(activity.is_quiet(later));
    assert_eq!(activity.label(later), "No activity · 45s");
    assert!(activity.description(later).contains("runtime event 0s ago"));
    observe(&mut tracker, id, delta("turn", "hello"), later);
    assert!(!tracker.get(id).is_quiet(later));
}

#[test]
fn agents_and_view_changes_have_independent_clocks() {
    let mut tracker = Tracker::default();
    let parent = ProcessId::new();
    let child = ProcessId::new();
    let now = Instant::now();
    start(&mut tracker, parent, now);
    start(&mut tracker, child, now);
    let later = now + Duration::from_secs(45);
    observe(&mut tracker, child, delta("turn", "busy"), later);
    assert!(tracker.get(parent).is_quiet(later));
    assert!(!tracker.get(child).is_quiet(later));
    let first = tracker.snapshot(Some(parent), true);
    assert_eq!(first.others[0].0, child);
    let switched = tracker.snapshot(Some(child), false);
    assert!(switched.others[0].1.is_quiet(later));
    assert_eq!(tracker.snapshot(Some(parent), true), first);
    tracker.clear();
    assert!(tracker.snapshot(Some(parent), true).others.is_empty());
    assert_eq!(tracker.get(parent).phase, Phase::Idle);
}

#[test]
fn parallel_tools_and_waits_keep_their_reason_until_all_finish() {
    let mut tracker = Tracker::default();
    let id = ProcessId::new();
    let now = Instant::now();
    start(&mut tracker, id, now);
    for call_id in ["one", "two"] {
        observe(
            &mut tracker,
            id,
            EventMsg::WebSearchBegin(WebSearchBeginEvent {
                call_id: call_id.into(),
            }),
            now,
        );
    }
    assert_eq!(tracker.get(id).phase, Phase::Tools);
    let later = now + Duration::from_secs(45);
    assert_eq!(tracker.get(id).label(later), "Tools · no activity 45s");
    observe(
        &mut tracker,
        id,
        EventMsg::WebSearchEnd(WebSearchEndEvent {
            call_id: "one".into(),
            query: String::new(),
            action: chaos_ipc::models::WebSearchAction::Other,
        }),
        later,
    );
    assert_eq!(tracker.get(id).phase, Phase::Tools);
    observe(
        &mut tracker,
        id,
        EventMsg::CollabWaitingBegin(CollabWaitingBeginEvent {
            sender_process_id: id,
            receiver_process_ids: vec![],
            receiver_agents: vec![],
            call_id: "wait".into(),
        }),
        later,
    );
    assert_eq!(tracker.get(id).phase, Phase::WaitingAgents);
    assert!(!tracker.get(id).is_quiet(later + Duration::from_secs(500)));
    observe(
        &mut tracker,
        id,
        EventMsg::CollabWaitingEnd(CollabWaitingEndEvent {
            sender_process_id: id,
            call_id: "wait".into(),
            agent_statuses: vec![],
            statuses: HashMap::new(),
        }),
        later,
    );
    assert_eq!(tracker.get(id).phase, Phase::Tools);
}

#[test]
fn concurrent_input_prompts_are_not_cleared_by_a_single_answer() {
    let mut tracker = Tracker::default();
    let id = ProcessId::new();
    let now = Instant::now();
    start(&mut tracker, id, now);
    for call_id in ["one", "two"] {
        observe(
            &mut tracker,
            id,
            EventMsg::RequestUserInput(chaos_ipc::request_user_input::RequestUserInputEvent {
                call_id: call_id.into(),
                turn_id: "turn".into(),
                questions: vec![],
            }),
            now,
        );
    }
    let answer = Op::UserInputAnswer {
        id: "turn".into(),
        response: chaos_ipc::request_user_input::RequestUserInputResponse {
            answers: HashMap::new(),
        },
    };
    let later = now + Duration::from_secs(500);
    tracker.note_op(id, &answer, later);
    assert_eq!(tracker.get(id).phase, Phase::NeedsInput);
    assert!(!tracker.get(id).is_quiet(now + Duration::from_secs(500)));
    tracker.note_op(id, &answer, later);
    assert_eq!(tracker.get(id).phase, Phase::Working);
    assert_eq!(
        tracker.get(id).last_activity,
        Some(now),
        "a user answer is not model progress"
    );
    assert!(
        !tracker.get(id).is_quiet(later),
        "give the resumed model a grace period"
    );
    assert!(tracker.get(id).is_quiet(later + QUIET_AFTER));
}

#[test]
fn stale_turns_terminal_events_and_reconnection() {
    let mut tracker = Tracker::default();
    let id = ProcessId::new();
    let now = Instant::now();
    start(&mut tracker, id, now);
    let later = now + Duration::from_secs(45);
    observe(&mut tracker, id, delta("previous-turn", "late"), later);
    observe(
        &mut tracker,
        id,
        EventMsg::RequestUserInput(chaos_ipc::request_user_input::RequestUserInputEvent {
            call_id: "stale-prompt".into(),
            turn_id: "previous-turn".into(),
            questions: vec![],
        }),
        later,
    );
    observe(
        &mut tracker,
        id,
        EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "previous-turn".into(),
            last_agent_message: None,
        }),
        later,
    );
    assert_eq!(tracker.get(id).last_activity, Some(now));
    assert_eq!(tracker.get(id).phase, Phase::Working);
    observe(
        &mut tracker,
        id,
        EventMsg::StreamError(StreamErrorEvent {
            message: "reconnecting".into(),
            chaos_error_info: None,
            additional_details: None,
        }),
        later,
    );
    assert_eq!(tracker.get(id).phase, Phase::Reconnecting);
    observe(&mut tracker, id, delta("turn", "recovered"), later);
    assert_eq!(tracker.get(id).phase, Phase::Working);
    observe(
        &mut tracker,
        id,
        EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some("turn".into()),
            reason: TurnAbortReason::Interrupted,
        }),
        later,
    );
    assert_eq!(tracker.get(id).phase, Phase::Interrupted);
    observe(&mut tracker, id, delta("turn", "late"), later);
    assert_eq!(tracker.get(id).phase, Phase::Interrupted);
    start(&mut tracker, id, later);
    observe(
        &mut tracker,
        id,
        EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn".into(),
            last_agent_message: None,
        }),
        later,
    );
    assert_eq!(tracker.get(id).phase, Phase::Idle);
    tracker.disconnected(id);
    assert_eq!(tracker.get(id).phase, Phase::Disconnected);
    tracker.closed(id);
    tracker.disconnected(id);
    assert_eq!(tracker.get(id).phase, Phase::Closed);
}

#[test]
fn repeated_running_reports_do_not_renew_a_child_and_close_waits_for_confirmation() {
    let mut tracker = Tracker::default();
    let child = ProcessId::new();
    let parent = ProcessId::new();
    let now = Instant::now();
    start(&mut tracker, child, now);
    tracker.reported_status(child, &AgentStatus::Running, now);
    assert!(tracker.get(child).is_quiet(now + Duration::from_secs(45)));
    observe(
        &mut tracker,
        parent,
        EventMsg::CollabCloseEnd(CollabCloseEndEvent {
            call_id: "close".into(),
            sender_process_id: parent,
            receiver_process_id: child,
            receiver_agent_nickname: None,
            receiver_agent_role: None,
            status: AgentStatus::Running,
        }),
        now,
    );
    assert_eq!(
        tracker.get(child).phase,
        Phase::Working,
        "close-end reports pre-close state even if shutdown fails"
    );
    observe(&mut tracker, child, EventMsg::ShutdownComplete, now);
    assert_eq!(tracker.get(child).phase, Phase::Closed);
}

#[test]
fn a_child_without_direct_events_ages_from_discovery_not_status_polls() {
    let mut tracker = Tracker::default();
    let child = ProcessId::new();
    let now = Instant::now();
    tracker.reported_status(child, &AgentStatus::PendingInit, now);
    assert_eq!(tracker.get(child).phase, Phase::Starting);
    let later = now + Duration::from_secs(45);
    tracker.reported_status(child, &AgentStatus::Running, later);
    assert!(tracker.get(child).is_quiet(later));
    assert_eq!(tracker.get(child).last_activity, None);
    assert_eq!(tracker.get(child).last_runtime_event, None);
}
