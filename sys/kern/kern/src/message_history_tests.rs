use super::*;
use crate::config::ConfigBuilder;
use crate::runtime_db;
use chaos_ipc::ProcessId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

async fn test_config_and_runtime_db() -> (
    TempDir,
    crate::config::Config,
    crate::runtime_db::RuntimeDbHandle,
) {
    let chaos_home = TempDir::new().expect("create temp dir");
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .build()
        .await
        .expect("load config");
    let runtime_db = runtime_db::init(&config)
        .await
        .expect("initialize runtime db");
    (chaos_home, config, runtime_db)
}

fn estimated_entry_bytes(entry: &HistoryEntry) -> u64 {
    let mut serialized = serde_json::to_string(entry).expect("serialize history entry");
    serialized.push('\n');
    serialized.len() as u64
}

#[tokio::test]
async fn lookup_reads_history_entries() {
    let (_home, _config, runtime_db) = test_config_and_runtime_db().await;

    let entries = vec![
        HistoryEntry {
            conversation_id: "first-session".to_string(),
            ts: 1,
            text: "first".to_string(),
        },
        HistoryEntry {
            conversation_id: "second-session".to_string(),
            ts: 2,
            text: "second".to_string(),
        },
    ];

    for entry in &entries {
        runtime_db
            .append_message_history_entry(entry, None)
            .await
            .expect("append history entry");
    }

    let (log_id, count) = history_metadata(Some(runtime_db.as_ref())).await;
    assert_eq!(count, entries.len());

    let second_entry = lookup(log_id, 1, Some(runtime_db.as_ref()))
        .await
        .expect("fetch second history entry");
    assert_eq!(second_entry, entries[1]);
}

#[tokio::test]
async fn lookup_uses_stable_log_id_after_appends() {
    let (_home, _config, runtime_db) = test_config_and_runtime_db().await;

    let initial = HistoryEntry {
        conversation_id: "first-session".to_string(),
        ts: 1,
        text: "first".to_string(),
    };
    let appended = HistoryEntry {
        conversation_id: "second-session".to_string(),
        ts: 2,
        text: "second".to_string(),
    };

    runtime_db
        .append_message_history_entry(&initial, None)
        .await
        .expect("append initial entry");

    let (log_id, count) = history_metadata(Some(runtime_db.as_ref())).await;
    assert_eq!(count, 1);

    runtime_db
        .append_message_history_entry(&appended, None)
        .await
        .expect("append history entry");

    let (log_id_after_append, count_after_append) =
        history_metadata(Some(runtime_db.as_ref())).await;
    assert_eq!(log_id_after_append, log_id);
    assert_eq!(count_after_append, 2);

    let fetched = lookup(log_id, 1, Some(runtime_db.as_ref()))
        .await
        .expect("lookup appended history entry");
    assert_eq!(fetched, appended);
}

#[tokio::test]
async fn append_entry_trims_history_when_beyond_max_bytes() {
    let (_home, mut config, runtime_db) = test_config_and_runtime_db().await;
    let conversation_id = ProcessId::new();

    let entry_one = "a".repeat(200);
    let entry_two = "b".repeat(200);

    append_entry(
        &entry_one,
        &conversation_id,
        Some(runtime_db.as_ref()),
        &config,
    )
    .await
    .expect("write first entry");

    let first_entry_len = estimated_entry_bytes(&HistoryEntry {
        conversation_id: conversation_id.to_string(),
        ts: 0,
        text: entry_one.clone(),
    });
    let limit_bytes = first_entry_len + 10;
    config.history.max_bytes = Some(usize::try_from(limit_bytes).expect("limit fits"));

    append_entry(
        &entry_two,
        &conversation_id,
        Some(runtime_db.as_ref()),
        &config,
    )
    .await
    .expect("write second entry");

    let (log_id, count) = history_metadata(Some(runtime_db.as_ref())).await;
    assert_eq!(count, 1);
    let remaining = lookup(log_id, 0, Some(runtime_db.as_ref()))
        .await
        .expect("fetch surviving history entry");
    assert_eq!(remaining.text, entry_two);
}

#[tokio::test]
async fn append_entry_trims_history_to_soft_cap() {
    let (_home, mut config, runtime_db) = test_config_and_runtime_db().await;
    let conversation_id = ProcessId::new();

    let short_entry = "a".repeat(200);
    let long_entry = "b".repeat(400);

    let short_entry_record = HistoryEntry {
        conversation_id: conversation_id.to_string(),
        ts: 0,
        text: short_entry.clone(),
    };
    let long_entry_record = HistoryEntry {
        conversation_id: conversation_id.to_string(),
        ts: 0,
        text: long_entry.clone(),
    };

    let short_entry_len = estimated_entry_bytes(&short_entry_record);
    let long_entry_len = estimated_entry_bytes(&long_entry_record);

    append_entry(
        &short_entry,
        &conversation_id,
        Some(runtime_db.as_ref()),
        &config,
    )
    .await
    .expect("write first entry");
    append_entry(
        &long_entry,
        &conversation_id,
        Some(runtime_db.as_ref()),
        &config,
    )
    .await
    .expect("write second entry");

    config.history.max_bytes = Some(
        usize::try_from((2 * long_entry_len) + (short_entry_len / 2))
            .expect("max bytes should fit"),
    );

    append_entry(
        &long_entry,
        &conversation_id,
        Some(runtime_db.as_ref()),
        &config,
    )
    .await
    .expect("write third entry");

    let (log_id, count) = history_metadata(Some(runtime_db.as_ref())).await;
    assert_eq!(count, 1);
    let remaining = lookup(log_id, 0, Some(runtime_db.as_ref()))
        .await
        .expect("fetch surviving history entry");
    assert_eq!(remaining.text, long_entry);
}
