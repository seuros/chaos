use super::*;
use chaos_ipc::ProcessId;
use chaos_kern::activity::{
    Activity, ActivityEvent, Attention as Reason, CallOutcome, MachineCause, Station,
};

fn apply(board: &mut Board, process_id: ProcessId, at_ms: i64, activity: Activity) {
    board.apply(&ActivityEvent {
        process_id,
        at_ms,
        activity,
    });
}

fn titles(board: &Board, now_ms: i64) -> Vec<String> {
    Attention::from(board, now_ms)
        .items
        .into_iter()
        .map(|item| item.title)
        .collect()
}

#[test]
fn a_quiet_resident_is_all_clear() {
    let mut board = Board::default();
    apply(&mut board, ProcessId::new(), 0, Activity::TurnStarted);
    assert!(titles(&board, 1_000).is_empty());
}

#[test]
fn the_worst_comes_first() {
    let mut board = Board::default();
    let id = ProcessId::new();
    apply(
        &mut board,
        id,
        0,
        Activity::CallStarted {
            call_id: "slow".into(),
            station: Station::Shell,
            label: "cargo".into(),
            check: None,
        },
    );
    apply(
        &mut board,
        id,
        1,
        Activity::NeedsYou {
            ask_id: "ask".into(),
            reason: Reason::CommandApproval,
        },
    );
    for at in 2..5 {
        apply(
            &mut board,
            id,
            at,
            Activity::CallEnded {
                call_id: format!("call-{at}"),
                station: Station::Network,
                label: "curl".into(),
                outcome: CallOutcome::Failed,
                duration_ms: None,
                check: None,
            },
        );
    }
    apply(
        &mut board,
        ProcessId::new(),
        5,
        Activity::Machine {
            phase: chaos_kern::activity::MachinePhase::Warning,
            causes: vec![MachineCause::Disk],
        },
    );
    assert_eq!(
        titles(&board, 61_000),
        vec![
            "agents paused",
            "waiting on you",
            "3 failures in a row",
            "slow call"
        ]
    );
    assert!(
        titles(&board, 30_000)
            .iter()
            .all(|title| title != "slow call"),
        "a call is slow only after a minute"
    );
}

#[test]
fn a_minion_failing_while_its_parent_waits_is_flagged() {
    let mut board = Board::default();
    let parent = ProcessId::new();
    let child = ProcessId::new();
    apply(
        &mut board,
        parent,
        0,
        Activity::AgentSpawned {
            child,
            nickname: Some("Ada".into()),
            role: None,
        },
    );
    apply(
        &mut board,
        parent,
        1,
        Activity::WaitingOnAgents {
            children: vec![child],
        },
    );
    apply(
        &mut board,
        parent,
        2,
        Activity::AgentState {
            child,
            state: chaos_kern::activity::AgentRun::Failed,
        },
    );
    let items = Attention::from(&board, 3).items;
    assert_eq!(items[0].title, "minion failed");
    assert_eq!(items[0].detail, "Ada failed while its parent waits");
    assert!(
        titles(&board, 6 * 60_000).contains(&"long wait on minions".to_string()),
        "{:?}",
        titles(&board, 6 * 60_000)
    );
}
