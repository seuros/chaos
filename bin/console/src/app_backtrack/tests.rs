use super::*;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;
use ratatui::prelude::Line;
use std::sync::Arc;

fn user(message: &str) -> Arc<dyn HistoryCell> {
    Arc::new(UserHistoryCell {
        message: message.to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    })
}

fn agent(message: &str, first: bool) -> Arc<dyn HistoryCell> {
    Arc::new(AgentMessageCell::new(
        vec![Line::from(message.to_string())],
        first,
    ))
}

fn agent_text(cell: &Arc<dyn HistoryCell>) -> String {
    let agent = cell
        .as_any()
        .downcast_ref::<AgentMessageCell>()
        .expect("agent cell");
    let agent_lines = agent.display_lines(u16::MAX);
    assert_eq!(agent_lines.len(), 1);
    agent_lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn user_message(cell: &Arc<dyn HistoryCell>) -> &str {
    &cell
        .as_any()
        .downcast_ref::<UserHistoryCell>()
        .expect("user cell")
        .message
}

pub(crate) fn app_backtrack_suite() {
    trim_to_nth_user_covers_first_and_later_selections();
    drop_last_n_user_turns_covers_normal_and_overflow_rollback_depths();
}

fn trim_to_nth_user_covers_first_and_later_selections() {
    let mut cells = vec![user("first user"), agent("assistant", true)];
    assert!(trim_transcript_cells_to_nth_user(&mut cells, 0));
    assert!(cells.is_empty());

    let mut cells = vec![agent("intro", true), user("first"), agent("after", false)];
    assert!(trim_transcript_cells_to_nth_user(&mut cells, 0));
    assert_eq!(cells.len(), 1);
    assert_eq!(agent_text(&cells[0]), "• intro");

    let mut cells = vec![
        agent("intro", true),
        user("first"),
        agent("between", false),
        user("second"),
        agent("tail", false),
    ];
    assert!(trim_transcript_cells_to_nth_user(&mut cells, 1));
    assert_eq!(cells.len(), 3);
    assert_eq!(agent_text(&cells[0]), "• intro");
    assert_eq!(user_message(&cells[1]), "first");
    assert_eq!(agent_text(&cells[2]), "  between");
}

fn drop_last_n_user_turns_covers_normal_and_overflow_rollback_depths() {
    let mut cells = vec![
        user("first"),
        agent("after first", false),
        user("second"),
        agent("after second", false),
    ];
    assert!(trim_transcript_cells_drop_last_n_user_turns(&mut cells, 1));
    assert_eq!(cells.len(), 2);
    assert_eq!(user_message(&cells[0]), "first");

    let mut cells = vec![agent("intro", true), user("first"), agent("after", false)];
    assert!(trim_transcript_cells_drop_last_n_user_turns(
        &mut cells,
        u32::MAX
    ));
    assert_eq!(cells.len(), 1);
    assert_eq!(agent_text(&cells[0]), "• intro");
}
