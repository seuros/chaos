use super::*;
use chaos_kern::activity::{Activity, ActivityEvent, Station};
use rama::http::sse::EventDataWrite as _;

fn frame(patch: Patch) -> String {
    let mut frame = Vec::new();
    patch
        .unwrap()
        .into_data()
        .unwrap()
        .write_data(&mut frame)
        .unwrap();
    String::from_utf8(frame).unwrap()
}

#[test]
fn a_session_change_replaces_only_its_card() {
    let mut board = Board::default();
    let id = chaos_ipc::ProcessId::new();
    board.apply(&ActivityEvent {
        process_id: id,
        at_ms: 1,
        activity: Activity::CallStarted {
            call_id: "call-1".into(),
            station: Station::Shell,
            label: "cargo".into(),
            check: None,
        },
    });
    let frame = frame(replace(&Session::from(&board, id, &board.sessions[&id])));
    assert!(frame.contains(&format!(r#"id="s-{id}""#)), "{frame}");
    assert!(!frame.contains("selector"), "patched by id: {frame}");
    assert!(!frame.contains("w-sessions"), "{frame}");
}

#[test]
fn a_new_feed_entry_goes_first() {
    let mut board = Board::default();
    board.apply(&ActivityEvent {
        process_id: chaos_ipc::ProcessId::new(),
        at_ms: 1,
        activity: Activity::Compacted,
    });
    let entry = widgets::Entry::from(&board, board.feed.back().unwrap());
    let frame = frame(prepend("#w-feed-list", &entry));
    assert!(frame.contains("selector #w-feed-list"), "{frame}");
    assert!(frame.contains("mode prepend"), "{frame}");
    assert!(frame.contains("context compacted"), "{frame}");
}

#[test]
fn the_stylesheet_answers_a_matching_etag_with_not_modified() {
    let fresh = stylesheet_response(None);
    assert_eq!(fresh.status(), StatusCode::OK);
    let etag = fresh.headers().get(ETAG).unwrap().clone();
    assert_eq!(
        stylesheet_response(Some(&etag)).status(),
        StatusCode::NOT_MODIFIED
    );
}
