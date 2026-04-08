use super::*;
use crate::rollout::list::parse_cursor;
use jiff::Timestamp;
use pretty_assertions::assert_eq;

#[test]
fn cursor_to_anchor_normalizes_timestamp_format() {
    let uuid = Uuid::new_v4();
    let ts_str = "2026-01-27T12-34-56";
    let token = format!("{ts_str}|{uuid}");
    let cursor = parse_cursor(token.as_str()).expect("cursor should parse");
    let anchor = cursor_to_anchor(Some(&cursor)).expect("anchor should parse");

    let expected_ts: Timestamp = "2026-01-27T12:34:56Z".parse().expect("ts should parse");

    assert_eq!(anchor.id, uuid);
    assert_eq!(anchor.ts, expected_ts);
}
