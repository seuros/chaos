use super::*;
use chaos_kern::activity::{AgentRun, CheckCounts};

fn event(process_id: ProcessId, at_ms: i64, activity: Activity) -> ActivityEvent {
    ActivityEvent {
        process_id,
        at_ms,
        activity,
    }
}

fn call_ended(check: Option<Check>, outcome: CallOutcome) -> Activity {
    Activity::CallEnded {
        call_id: "call-1".into(),
        station: Station::Shell,
        label: "cargo".into(),
        outcome,
        duration_ms: Some(1_500),
        check,
    }
}

#[test]
fn a_turn_works_through_its_calls_and_settles() {
    let mut board = Board::default();
    let id = ProcessId::new();
    assert_eq!(
        board.apply(&event(id, 1, Activity::TurnStarted)),
        vec![Change::SessionAdded]
    );
    board.apply(&event(
        id,
        2,
        Activity::CallStarted {
            call_id: "call-1".into(),
            station: Station::Shell,
            label: "cargo".into(),
            check: Some(CheckKind::Test),
        },
    ));
    let session = &board.sessions[&id];
    assert!(session.working);
    assert_eq!(session.calls.len(), 1);

    let check = Check {
        kind: CheckKind::Test,
        counts: Some(CheckCounts {
            passed: 9,
            failed: 1,
            skipped: 0,
        }),
    };
    assert_eq!(
        board.apply(&event(id, 3, call_ended(Some(check), CallOutcome::Ok))),
        vec![Change::Session(id), Change::Map, Change::Feed]
    );
    let entry = board.feed.back().unwrap();
    assert_eq!(entry.text, "cargo · 9 passed, 1 failed · 1.5s");
    assert_eq!(
        entry.tone,
        Tone::Bad,
        "failed tests behind exit 0 still read bad"
    );
    assert_eq!(board.sessions[&id].last_check, Some(check));
    assert_eq!(board.stations[&Station::Shell].done, 1);

    board.apply(&event(
        id,
        4,
        Activity::TurnEnded {
            outcome: TurnOutcome::Completed,
        },
    ));
    let session = &board.sessions[&id];
    assert!(!session.working && session.calls.is_empty());
    assert_eq!(session.turns, 1);
}

#[test]
fn an_ask_waits_until_it_is_resolved() {
    let mut board = Board::default();
    let id = ProcessId::new();
    board.apply(&event(
        id,
        1,
        Activity::NeedsYou {
            ask_id: "ask-1".into(),
            reason: Attention::PatchApproval,
        },
    ));
    assert_eq!(board.feed.back().unwrap().text, "needs you: patch approval");
    assert_eq!(board.sessions[&id].asks.len(), 1);
    board.apply(&event(
        id,
        2,
        Activity::Resolved {
            ask_id: "ask-1".into(),
        },
    ));
    assert!(board.sessions[&id].asks.is_empty());
}

#[test]
fn host_wide_state_does_not_invent_a_session() {
    let mut board = Board::default();
    let changes = board.apply(&event(
        ProcessId::new(),
        1,
        Activity::Machine {
            phase: MachinePhase::Warning,
            causes: vec![MachineCause::Thermal],
        },
    ));
    assert_eq!(changes, vec![Change::Machine]);
    assert!(board.sessions.is_empty());
    assert_eq!(
        board.machine,
        Some((MachinePhase::Warning, vec![MachineCause::Thermal]))
    );
}

#[test]
fn the_feed_and_the_session_list_stay_bounded() {
    let mut board = Board::default();
    let busy = ProcessId::new();
    board.apply(&event(busy, 0, Activity::TurnStarted));
    for at in 1..=(SESSIONS_LEN.max(FEED_LEN) as i64 + 10) {
        let id = ProcessId::new();
        board.apply(&event(id, at, call_ended(None, CallOutcome::Failed)));
    }
    assert_eq!(board.sessions.len(), SESSIONS_LEN);
    assert!(board.sessions.contains_key(&busy), "working sessions stay");
    assert_eq!(board.feed.len(), FEED_LEN);
}

#[test]
fn durations_read_at_a_glance() {
    assert_eq!(duration(42), "42ms");
    assert_eq!(duration(1_500), "1.5s");
    assert_eq!(duration(125_000), "2m05s");
}

#[test]
fn failures_in_a_row_and_failed_checks_clear_when_work_succeeds() {
    let mut board = Board::default();
    let id = ProcessId::new();
    for at in 1..=3 {
        board.apply(&event(id, at, call_ended(None, CallOutcome::Failed)));
    }
    assert_eq!(board.sessions[&id].failing, 3);
    let failed = Check {
        kind: CheckKind::Test,
        counts: Some(CheckCounts {
            passed: 1,
            failed: 2,
            skipped: 0,
        }),
    };
    board.apply(&event(id, 4, call_ended(Some(failed), CallOutcome::Ok)));
    assert_eq!(board.sessions[&id].failed_check, Some(CheckKind::Test));
    let passed = Check {
        kind: CheckKind::Test,
        counts: Some(CheckCounts {
            passed: 3,
            failed: 0,
            skipped: 0,
        }),
    };
    board.apply(&event(id, 5, call_ended(Some(passed), CallOutcome::Ok)));
    let session = &board.sessions[&id];
    assert_eq!((session.failing, session.failed_check), (0, None));
    assert_eq!(board.stations[&Station::Shell].failed_at_ms, Some(3));
}

#[test]
fn the_route_collapses_repeat_visits_and_keeps_the_last_stops() {
    let mut board = Board::default();
    let id = ProcessId::new();
    let stations = [
        Station::Files,
        Station::Files,
        Station::Shell,
        Station::Network,
        Station::Shell,
        Station::Files,
        Station::Edit,
        Station::Shell,
        Station::Kernel,
        Station::Files,
    ];
    for (at, station) in stations.into_iter().enumerate() {
        board.apply(&event(
            id,
            at as i64,
            Activity::CallStarted {
                call_id: format!("call-{at}"),
                station,
                label: "x".into(),
                check: None,
            },
        ));
    }
    let route: Vec<_> = board.route.iter().map(|(station, _)| *station).collect();
    assert_eq!(route.len(), ROUTE_LEN);
    assert_eq!(route.first(), Some(&Station::Network));
    assert_eq!(route.last(), Some(&Station::Files));
}

#[test]
fn a_minion_knows_its_parent_and_the_parent_tracks_it() {
    let mut board = Board::default();
    let parent = ProcessId::new();
    let child = ProcessId::new();
    board.apply(&event(parent, 1, Activity::TurnStarted));
    let changes = board.apply(&event(
        parent,
        2,
        Activity::AgentSpawned {
            child,
            nickname: Some("Ada".into()),
            role: Some("reviewer".into()),
        },
    ));
    assert!(changes.contains(&Change::SessionAdded), "{changes:?}");
    assert_eq!(board.sessions[&child].parent, Some(parent));
    assert_eq!(board.label(&child), "Ada");

    board.apply(&event(
        parent,
        3,
        Activity::WaitingOnAgents {
            children: vec![child],
        },
    ));
    board.apply(&event(
        parent,
        4,
        Activity::AgentState {
            child,
            state: AgentRun::Failed,
        },
    ));
    let minion = &board.sessions[&parent].minions[0];
    assert_eq!(minion.state, AgentRun::Failed);
    assert_eq!(board.sessions[&parent].waiting_on, vec![child]);

    board.apply(&event(parent, 5, Activity::DoneWaiting));
    board.apply(&event(parent, 6, Activity::AgentEnded { child }));
    assert!(board.sessions[&parent].waiting_on.is_empty());
    assert!(board.sessions[&child].closed);
    assert_eq!(
        board.sessions[&parent].minions[0].state,
        AgentRun::Failed,
        "closing keeps how it ended"
    );
}
