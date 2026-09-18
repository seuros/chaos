use std::path::PathBuf;

use chaos_ipc::protocol::HookEventName;

use super::ConfiguredHandler;
use super::select_handlers;

fn make_handler(
    event_name: HookEventName,
    matcher: Option<&str>,
    command: &str,
    display_order: i64,
) -> ConfiguredHandler {
    ConfiguredHandler {
        event_name,
        matcher: matcher.map(str::to_owned),
        command: command.to_string(),
        timeout_sec: 5,
        status_message: None,
        source_path: PathBuf::from("/tmp/hooks.json"),
        display_order,
    }
}

#[test]
fn select_handlers_keeps_duplicate_stop_handlers() {
    let handlers = vec![
        make_handler(HookEventName::Stop, None, "echo same", 0),
        make_handler(HookEventName::Stop, None, "echo same", 1),
    ];

    let selected = select_handlers(&handlers, HookEventName::Stop, None);

    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].display_order, 0);
    assert_eq!(selected[1].display_order, 1);
}

#[test]
fn select_handlers_keeps_overlapping_session_start_matchers() {
    let handlers = vec![
        make_handler(HookEventName::SessionStart, Some("start.*"), "echo same", 0),
        make_handler(
            HookEventName::SessionStart,
            Some("^startup$"),
            "echo same",
            1,
        ),
    ];

    let selected = select_handlers(&handlers, HookEventName::SessionStart, Some("startup"));

    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].display_order, 0);
    assert_eq!(selected[1].display_order, 1);
}

#[test]
fn select_handlers_preserves_declaration_order() {
    let handlers = vec![
        make_handler(HookEventName::Stop, None, "first", 0),
        make_handler(HookEventName::Stop, None, "second", 1),
        make_handler(HookEventName::Stop, None, "third", 2),
    ];

    let selected = select_handlers(&handlers, HookEventName::Stop, None);

    assert_eq!(selected.len(), 3);
    assert_eq!(selected[0].command, "first");
    assert_eq!(selected[1].command, "second");
    assert_eq!(selected[2].command, "third");
}
