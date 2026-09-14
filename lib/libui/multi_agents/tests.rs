use super::*;
use crate::history_cell::HistoryCell;
#[cfg(target_os = "macos")]
use crossterm::event::KeyEvent;
#[cfg(target_os = "macos")]
use crossterm::event::KeyModifiers;
macro_rules! assert_snapshot {
    ($($arg:tt)*) => {
        insta::with_settings!({ snapshot_path => "../snapshots" }, {
            insta::assert_snapshot!($($arg)*);
        })
    };
}
use pretty_assertions::assert_eq;
use ratatui::style::Modifier;

fn collab_events_snapshot() {
    let sender_process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000001")
        .expect("valid sender thread id");
    let robie_id = ProcessId::from_string("00000000-0000-0000-0000-000000000002")
        .expect("valid robie thread id");
    let bob_id = ProcessId::from_string("00000000-0000-0000-0000-000000000003")
        .expect("valid bob thread id");

    let spawn = spawn_end(
        CollabAgentSpawnEndEvent {
            call_id: "call-spawn".to_string(),
            sender_process_id,
            new_process_id: Some(robie_id),
            new_agent_nickname: Some("Robie".to_string()),
            new_agent_role: Some("scout".to_string()),
            prompt: "Compute 11! and reply with just the integer result.".to_string(),
            model: "gpt-5".to_string(),
            reasoning_effort: ReasoningEffortConfig::High,
            status: AgentStatus::PendingInit,
        },
        Some(&SpawnRequestSummary {
            model: "gpt-5".to_string(),
            reasoning_effort: ReasoningEffortConfig::High,
        }),
    );

    let send = interaction_end(CollabAgentInteractionEndEvent {
        call_id: "call-send".to_string(),
        sender_process_id,
        receiver_process_id: robie_id,
        receiver_agent_nickname: Some("Robie".to_string()),
        receiver_agent_role: Some("scout".to_string()),
        prompt: "Please continue and return the answer only.".to_string(),
        status: AgentStatus::Running,
    });

    let waiting = waiting_begin(CollabWaitingBeginEvent {
        sender_process_id,
        receiver_process_ids: vec![robie_id],
        receiver_agents: vec![CollabAgentRef {
            process_id: robie_id,
            agent_nickname: Some("Robie".to_string()),
            agent_role: Some("scout".to_string()),
        }],
        call_id: "call-wait".to_string(),
    });

    let mut statuses = HashMap::new();
    statuses.insert(
        robie_id,
        AgentStatus::Completed(Some("39916800".to_string())),
    );
    statuses.insert(bob_id, AgentStatus::Errored("tool timeout".to_string()));
    let finished = waiting_end(CollabWaitingEndEvent {
        sender_process_id,
        call_id: "call-wait".to_string(),
        agent_statuses: vec![
            CollabAgentStatusEntry {
                process_id: robie_id,
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("scout".to_string()),
                status: AgentStatus::Completed(Some("39916800".to_string())),
            },
            CollabAgentStatusEntry {
                process_id: bob_id,
                agent_nickname: Some("Bob".to_string()),
                agent_role: Some("task".to_string()),
                status: AgentStatus::Errored("tool timeout".to_string()),
            },
        ],
        statuses,
    });

    let close = close_end(CollabCloseEndEvent {
        call_id: "call-close".to_string(),
        sender_process_id,
        receiver_process_id: robie_id,
        receiver_agent_nickname: Some("Robie".to_string()),
        receiver_agent_role: Some("scout".to_string()),
        status: AgentStatus::Completed(Some("39916800".to_string())),
    });

    let snapshot = [spawn, send, waiting, finished, close]
        .iter()
        .map(cell_to_text)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_snapshot!("collab_agent_transcript", snapshot);
}

#[cfg(target_os = "macos")]
fn agent_shortcut_matches_option_arrow_word_motion_fallbacks_only_when_allowed() {
    assert!(previous_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
        false,
    ));
    assert!(next_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
        false,
    ));
    assert!(previous_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
        true,
    ));
    assert!(next_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
        true,
    ));
    assert!(!previous_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
        false,
    ));
    assert!(!next_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
        false,
    ));
}

#[cfg(not(target_os = "macos"))]
fn agent_shortcut_matches_option_arrows_only() {
    assert!(previous_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Left, crossterm::event::KeyModifiers::ALT,),
        false
    ));
    assert!(next_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Right, crossterm::event::KeyModifiers::ALT,),
        false
    ));
    assert!(!previous_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Char('b'), crossterm::event::KeyModifiers::ALT,),
        false
    ));
    assert!(!next_agent_shortcut_matches(
        KeyEvent::new(KeyCode::Char('f'), crossterm::event::KeyModifiers::ALT,),
        false
    ));
}

pub(crate) fn multi_agents_suite() {
    collab_events_snapshot();
    #[cfg(target_os = "macos")]
    agent_shortcut_matches_option_arrow_word_motion_fallbacks_only_when_allowed();
    #[cfg(not(target_os = "macos"))]
    agent_shortcut_matches_option_arrows_only();
    title_styles_nickname_and_role();
    collab_resume_interrupted_snapshot();
}
#[cfg(test)]
fn title_styles_nickname_and_role() {
    let sender_process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000001")
        .expect("valid sender thread id");
    let robie_id = ProcessId::from_string("00000000-0000-0000-0000-000000000002")
        .expect("valid robie thread id");
    let cell = spawn_end(
        CollabAgentSpawnEndEvent {
            call_id: "call-spawn".to_string(),
            sender_process_id,
            new_process_id: Some(robie_id),
            new_agent_nickname: Some("Robie".to_string()),
            new_agent_role: Some("scout".to_string()),
            prompt: String::new(),
            model: "gpt-5".to_string(),
            reasoning_effort: ReasoningEffortConfig::High,
            status: AgentStatus::PendingInit,
        },
        Some(&SpawnRequestSummary {
            model: "gpt-5".to_string(),
            reasoning_effort: ReasoningEffortConfig::High,
        }),
    );

    let lines = cell.display_lines(200);
    let title = &lines[0];
    assert_eq!(title.spans[2].content.as_ref(), "Robie");
    assert_eq!(title.spans[2].style.fg, Some(crate::theme::accent_color()));
    assert!(title.spans[2].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(title.spans[4].content.as_ref(), "[scout]");
    assert_eq!(title.spans[4].style.fg, None);
    assert!(!title.spans[4].style.add_modifier.contains(Modifier::DIM));
    assert_eq!(title.spans[6].content.as_ref(), "(gpt-5 high)");
    assert_eq!(
        title.spans[6].style.fg,
        Some(crate::theme::annotation_color())
    );
}

#[cfg(test)]
fn collab_resume_interrupted_snapshot() {
    let sender_process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000001")
        .expect("valid sender thread id");
    let robie_id = ProcessId::from_string("00000000-0000-0000-0000-000000000002")
        .expect("valid robie thread id");

    let cell = resume_end(CollabResumeEndEvent {
        call_id: "call-resume".to_string(),
        sender_process_id,
        receiver_process_id: robie_id,
        receiver_agent_nickname: Some("Robie".to_string()),
        receiver_agent_role: Some("scout".to_string()),
        status: AgentStatus::Interrupted,
    });

    assert_snapshot!("collab_resume_interrupted", cell_to_text(&cell));
}

fn cell_to_text(cell: &PlainHistoryCell) -> String {
    cell.display_lines(200)
        .iter()
        .map(line_to_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_to_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("")
}
