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

#[tokio::test]
async fn open_runtime_db_with_config_uses_storage_url() {
    let chaos_home = tempfile::tempdir().expect("create chaos home");
    let storage_dir = tempfile::tempdir().expect("create storage dir");
    let db_path = storage_dir.path().join("configured.sqlite");
    let storage_url = format!("sqlite://{}", db_path.display());

    let runtime = open_or_create_runtime_db_with_config(
        Some(&storage_url),
        chaos_home.path(),
        "test-provider",
    )
    .await
    .expect("open runtime db from config storage_url");

    assert!(
        matches!(runtime, RuntimeDbHandle::Sqlite(_)),
        "sqlite storage_url should create a sqlite runtime"
    );
    assert!(
        tokio::fs::try_exists(&db_path)
            .await
            .expect("stat configured db"),
        "configured sqlite URL should create the target db"
    );
    assert!(
        !tokio::fs::try_exists(&chaos_proc::runtime_db_path(chaos_home.path()))
            .await
            .expect("stat fallback db"),
        "explicit storage_url should not fall back to chaos_home"
    );
}
