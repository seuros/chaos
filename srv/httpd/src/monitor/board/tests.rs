use super::*;
use chaos_kern::activity::CheckCounts;

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
        vec![Change::Session(id), Change::Stations, Change::Feed]
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
