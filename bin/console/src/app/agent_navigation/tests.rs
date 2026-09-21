use super::*;
use pretty_assertions::assert_eq;

fn populated_state() -> (AgentNavigationState, ProcessId, ProcessId, ProcessId) {
    let mut state = AgentNavigationState::default();
    let main_process_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000101").expect("valid thread");
    let first_agent_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000102").expect("valid thread");
    let second_agent_id =
        ProcessId::from_string("00000000-0000-0000-0000-000000000103").expect("valid thread");

    state.upsert(main_process_id, None, None, false);
    state.upsert(
        first_agent_id,
        Some("Robie".to_string()),
        Some("scout".to_string()),
        false,
    );
    state.upsert(
        second_agent_id,
        Some("Bob".to_string()),
        Some("task".to_string()),
        false,
    );

    (state, main_process_id, first_agent_id, second_agent_id)
}

pub(crate) fn agent_navigation_state_preserves_order_wraps_and_formats_labels() {
    let (mut state, main_process_id, first_agent_id, second_agent_id) = populated_state();

    state.upsert(
        first_agent_id,
        Some("Robie".to_string()),
        Some("task".to_string()),
        true,
    );

    assert_eq!(
        state.ordered_process_ids(),
        vec![main_process_id, first_agent_id, second_agent_id]
    );

    assert_eq!(
        state.adjacent_process_id(Some(second_agent_id), AgentNavigationDirection::Next),
        Some(main_process_id)
    );
    assert_eq!(
        state.adjacent_process_id(Some(second_agent_id), AgentNavigationDirection::Previous),
        Some(first_agent_id)
    );
    assert_eq!(
        state.adjacent_process_id(Some(main_process_id), AgentNavigationDirection::Previous),
        Some(second_agent_id)
    );

    let previous: Span<'static> = previous_agent_shortcut().into();
    let next: Span<'static> = next_agent_shortcut().into();
    let subtitle = AgentNavigationState::picker_subtitle();

    assert!(subtitle.contains(previous.content.as_ref()));
    assert!(subtitle.contains(next.content.as_ref()));

    assert_eq!(
        state.active_agent_label(Some(first_agent_id), Some(main_process_id)),
        Some("Robie [task]".to_string())
    );
    assert_eq!(
        state.active_agent_label(Some(main_process_id), Some(main_process_id)),
        Some("Main [default]".to_string())
    );
    for width in [1, 12, 40, 100] {
        let area = ratatui::layout::Rect::new(3, 2, width, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let mut active = main_process_id;
        for direction in [
            AgentNavigationDirection::Next,
            AgentNavigationDirection::Previous,
        ] {
            for _ in 0..9 {
                active = state.adjacent_process_id(Some(active), direction).unwrap();
                state.render_tabs(area, &mut buf, Some(active), Some(main_process_id));
                assert!(state.tab_hits.iter().any(|(id, _)| *id == active));
                for (id, rect) in &state.tab_hits {
                    assert!(area.contains(rect.as_position()));
                    assert!(rect.right() <= area.right());
                    assert_eq!(state.tab_at(rect.as_position()), Some(*id));
                }
            }
        }
        assert_eq!(state.tab_at((area.x, area.y + 1).into()), None);
    }
    state.clear();
    assert!(!state.has_tabs());
    assert!(!state.tabs_contain((3, 2).into()));
}
