use chaos_ipc::ProcessId;
use chaos_kern::activity::{Activity, ActivityEvent, McpState, Station};

use super::Session;
use crate::monitor::board::Board;

fn apply(board: &mut Board, id: ProcessId, at_ms: i64, activity: Activity) {
    board.apply(&ActivityEvent {
        process_id: id,
        at_ms,
        activity,
    });
}

fn server(name: &str, state: McpState) -> Activity {
    Activity::McpServer {
        server: name.into(),
        state,
    }
}

fn chips(board: &Board, id: ProcessId) -> Vec<(String, String, bool)> {
    Session::from(board, id, &board.sessions[&id])
        .mcp
        .into_iter()
        .map(|chip| (chip.name, chip.state, chip.busy))
        .collect()
}

#[test]
fn an_mcp_server_lights_up_only_while_its_tool_runs() {
    let mut board = Board::default();
    let id = ProcessId::new();
    apply(&mut board, id, 1, server("sky", McpState::Ready));
    apply(&mut board, id, 1, server("skipper", McpState::Ready));
    apply(
        &mut board,
        id,
        2,
        Activity::CallStarted {
            call_id: "call-1".into(),
            station: Station::Mcp,
            label: "skipper.git_status".into(),
            check: None,
        },
    );
    assert_eq!(
        chips(&board, id),
        vec![
            ("skipper".into(), "ready".into(), true),
            ("sky".into(), "ready".into(), false),
        ]
    );
    apply(
        &mut board,
        id,
        3,
        Activity::CallEnded {
            call_id: "call-1".into(),
            station: Station::Mcp,
            label: "skipper.git_status".into(),
            outcome: chaos_kern::activity::CallOutcome::Ok,
            duration_ms: Some(40),
            check: None,
        },
    );
    assert!(chips(&board, id).iter().all(|(.., busy)| !busy));
}

#[test]
fn a_failed_mcp_server_is_news_once_not_every_heartbeat() {
    let mut board = Board::default();
    let id = ProcessId::new();
    apply(&mut board, id, 1, server("vein", McpState::Starting));
    for at in 2..5 {
        apply(&mut board, id, at, server("vein", McpState::Failed));
    }
    let failures: Vec<_> = board.feed.iter().map(|entry| entry.text.as_str()).collect();
    assert_eq!(failures, vec!["vein failed to start"]);
    assert_eq!(
        chips(&board, id),
        vec![("vein".into(), "failed".into(), false)]
    );
}
