use super::ScrollState;

pub(crate) fn wrap_navigation_and_visibility() {
    let mut s = ScrollState::new();
    let len = 10;
    let vis = 5;

    s.clamp_selection(len);
    assert_eq!(s.selected_idx, Some(0));
    s.ensure_visible(len, vis);
    assert_eq!(s.scroll_top, 0);

    s.move_up_wrap(len);
    s.ensure_visible(len, vis);
    assert_eq!(s.selected_idx, Some(len - 1));
    match s.selected_idx {
        Some(sel) => assert!(s.scroll_top <= sel),
        None => panic!("expected Some(selected_idx) after wrap"),
    }

    s.move_down_wrap(len);
    s.ensure_visible(len, vis);
    assert_eq!(s.selected_idx, Some(0));
    assert_eq!(s.scroll_top, 0);
}
