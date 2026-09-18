use super::*;
use crossterm::event::KeyModifiers;

#[test]
fn inspector_resource_navigation_does_not_retain_another_sessions_content() {
    let mut pane = InspectorPane::default();
    pane.set_process(Some(ProcessId::new()));
    pane.resources = vec![
        Resource {
            server: "mcp".into(),
            uri: "state://one".into(),
            name: "one".into(),
        },
        Resource {
            server: "mcp".into(),
            uri: "state://two".into(),
            name: "two".into(),
        },
    ];
    pane.content = "old resource".into();
    pane.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(pane.selected, 1);
    assert!(pane.content.is_empty());

    pane.content = "private session data".into();
    pane.set_process(Some(ProcessId::new()));
    assert!(pane.resources.is_empty());
    assert!(pane.content.is_empty());
    assert!(!pane.catalog_loaded);
    let text = plain_text("\u{1b}[2Jhello\u{7}\nworld", 9);
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{7}'));
    assert_eq!(text.chars().count(), 9);
}
