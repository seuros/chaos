use chaos_test_fixtures::TEST_MODEL;
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::CompactedItem;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use serde_json::json;
use tempfile::tempdir;

use super::SqliteJournalStore;
use crate::model::AppendBatchInput;
use crate::model::CreateProcessInput;
use crate::model::InitializeProcessInput;
use crate::model::JournalEntry;
use crate::store::JournalStore;

fn message(role: &str, text: &str) -> RolloutItem {
    let content = match role {
        "assistant" => vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        _ => vec![ContentItem::InputText {
            text: text.to_string(),
        }],
    };
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content,
        end_turn: None,
        phase: None,
    })
}

fn message_texts(items: &[JournalEntry]) -> Vec<String> {
    items
        .iter()
        .filter_map(|entry| match &entry.item {
            RolloutItem::ResponseItem(ResponseItem::Message { content, .. }) => {
                content.iter().find_map(|content| match content {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        Some(text.clone())
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn create_process_and_round_trip_journal() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let db_path = temp_dir.path().join("journal.sqlite");
    let store = SqliteJournalStore::open(&db_path)
        .await
        .unwrap_or_else(|err| panic!("open: {err}"));

    let process_id = ProcessId::new();
    let created_at = jiff::Timestamp::now();
    let process = store
        .create_process(CreateProcessInput {
            process_id,
            parent: None,
            source: SessionSource::Cli,
            cwd: temp_dir.path().to_path_buf(),
            created_at,
            title: Some("test journal".to_string()),
            model_provider: Some("openai".to_string()),
            cli_version: Some("47.0.0".to_string()),
        })
        .await
        .unwrap_or_else(|err| panic!("create_process: {err}"));

    assert_eq!(process.process_id, process_id);
    assert_eq!(process.title, "test journal");

    let lease = store
        .acquire_lease(&process_id, &"owner-1".to_string(), Duration::from_secs(30))
        .await
        .unwrap_or_else(|err| panic!("acquire_lease: {err}"));

    let first_item = RolloutItem::Compacted(CompactedItem {
        message: "hello".to_string(),
        replacement_history: None,
    });
    let append = store
        .append_batch(AppendBatchInput {
            process_id,
            owner_id: "owner-1".to_string(),
            lease_token: lease.lease_token.clone(),
            expected_next_seq: 0,
            items: vec![JournalEntry {
                seq: 0,
                recorded_at: jiff::Timestamp::now(),
                item: first_item.clone(),
            }],
        })
        .await
        .unwrap_or_else(|err| panic!("append_batch: {err}"));

    assert_eq!(append.next_seq, 1);

    let loaded = store
        .load_journal(&process_id)
        .await
        .unwrap_or_else(|err| panic!("load_journal: {err}"));
    assert_eq!(loaded.process_id, process_id);
    assert_eq!(loaded.next_seq, 1);
    assert_eq!(loaded.items.len(), 1);
    assert_eq!(loaded.items[0].seq, 0);
    let loaded_item_json = serde_json::to_string(&loaded.items[0].item)
        .unwrap_or_else(|err| panic!("serialize loaded item: {err}"));
    let first_item_json =
        serde_json::to_string(&first_item).unwrap_or_else(|err| panic!("serialize item: {err}"));
    assert_eq!(loaded_item_json, first_item_json);
}

#[tokio::test]
async fn process_metadata_uses_latest_turn_context_cwd() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let db_path = temp_dir.path().join("journal.sqlite");
    let initial_cwd = temp_dir.path().join("initial");
    let latest_cwd = temp_dir.path().join("latest");
    let store = SqliteJournalStore::open(&db_path)
        .await
        .unwrap_or_else(|err| panic!("open: {err}"));
    let process_id = ProcessId::new();

    store
        .create_process(CreateProcessInput {
            process_id,
            parent: None,
            source: SessionSource::Cli,
            cwd: initial_cwd,
            created_at: jiff::Timestamp::now(),
            title: Some("cwd metadata".to_string()),
            model_provider: Some("openai".to_string()),
            cli_version: Some("47.1.0".to_string()),
        })
        .await
        .unwrap_or_else(|err| panic!("create_process: {err}"));
    let lease = store
        .acquire_lease(
            &process_id,
            &"cwd-owner".to_string(),
            Duration::from_secs(30),
        )
        .await
        .unwrap_or_else(|err| panic!("acquire_lease: {err}"));
    let turn_context: RolloutItem = serde_json::from_value(json!({
        "type": "turn_context",
        "payload": {
            "cwd": latest_cwd,
            "approval_policy": "headless",
            "vfs_policy": { "kind": "unrestricted" },
            "socket_policy": "restricted",
            "model": TEST_MODEL,
            "model_provider": "openai",
            "summary": "auto"
        }
    }))
    .unwrap_or_else(|err| panic!("turn context: {err}"));

    store
        .append_batch(AppendBatchInput {
            process_id,
            owner_id: "cwd-owner".to_string(),
            lease_token: lease.lease_token,
            expected_next_seq: 0,
            items: vec![JournalEntry {
                seq: 0,
                recorded_at: jiff::Timestamp::now(),
                item: turn_context,
            }],
        })
        .await
        .unwrap_or_else(|err| panic!("append_batch: {err}"));

    let process = store
        .get_process(&process_id)
        .await
        .unwrap_or_else(|err| panic!("get_process: {err}"))
        .expect("process row missing");
    assert_eq!(process.cwd, latest_cwd);

    let listed = store
        .list_processes(Some(false))
        .await
        .unwrap_or_else(|err| panic!("list_processes: {err}"));
    let listed_process = listed
        .into_iter()
        .find(|process| process.process_id == process_id)
        .expect("listed process missing");
    assert_eq!(listed_process.cwd, latest_cwd);
}

#[tokio::test]
async fn initialized_journal_can_be_reopened_and_appended_after_resume() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let db_path = temp_dir.path().join("journal.sqlite");
    let process_id = ProcessId::new();
    let created_at = jiff::Timestamp::now();

    {
        let store = SqliteJournalStore::open(&db_path)
            .await
            .unwrap_or_else(|err| panic!("open initial store: {err}"));
        let result = store
            .initialize_process(InitializeProcessInput {
                create: CreateProcessInput {
                    process_id,
                    parent: None,
                    source: SessionSource::Cli,
                    cwd: temp_dir.path().to_path_buf(),
                    created_at,
                    title: Some("resume append regression".to_string()),
                    model_provider: Some("openai".to_string()),
                    cli_version: Some("47.0.0".to_string()),
                },
                owner_id: "owner-initial".to_string(),
                ttl_ms: 30_000,
                items: vec![JournalEntry {
                    seq: 0,
                    recorded_at: jiff::Timestamp::now(),
                    item: message("user", "before resume"),
                }],
            })
            .await
            .unwrap_or_else(|err| panic!("initialize_process: {err}"));
        store
            .release_lease(
                &process_id,
                &result.lease.owner_id,
                &result.lease.lease_token,
            )
            .await
            .unwrap_or_else(|err| panic!("release initial lease: {err}"));
    }

    {
        let store = SqliteJournalStore::open(&db_path)
            .await
            .unwrap_or_else(|err| panic!("reopen for resume append: {err}"));
        let loaded_before = store
            .load_journal(&process_id)
            .await
            .unwrap_or_else(|err| panic!("load before resume append: {err}"));
        assert_eq!(loaded_before.next_seq, 1);
        assert_eq!(message_texts(&loaded_before.items), vec!["before resume"]);

        let lease = store
            .acquire_lease(
                &process_id,
                &"owner-resumed".to_string(),
                Duration::from_secs(30),
            )
            .await
            .unwrap_or_else(|err| panic!("acquire resumed lease: {err}"));
        store
            .append_batch(AppendBatchInput {
                process_id,
                owner_id: "owner-resumed".to_string(),
                lease_token: lease.lease_token,
                expected_next_seq: loaded_before.next_seq,
                items: vec![JournalEntry {
                    seq: loaded_before.next_seq,
                    recorded_at: jiff::Timestamp::now(),
                    item: message("assistant", "after resume"),
                }],
            })
            .await
            .unwrap_or_else(|err| panic!("append resumed item: {err}"));
    }

    let reopened = SqliteJournalStore::open(&db_path)
        .await
        .unwrap_or_else(|err| panic!("final reopen: {err}"));
    let loaded_after = reopened
        .load_journal(&process_id)
        .await
        .unwrap_or_else(|err| panic!("load after resume append: {err}"));
    assert_eq!(loaded_after.next_seq, 2);
    assert_eq!(
        message_texts(&loaded_after.items),
        vec!["before resume", "after resume"]
    );
}

#[tokio::test]
async fn rejects_append_with_wrong_expected_next_seq() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let db_path = temp_dir.path().join("journal.sqlite");
    let store = SqliteJournalStore::open(&db_path)
        .await
        .unwrap_or_else(|err| panic!("open: {err}"));
    let process_id = ProcessId::new();
    store
        .create_process(CreateProcessInput {
            process_id,
            parent: None,
            source: SessionSource::Cli,
            cwd: temp_dir.path().to_path_buf(),
            created_at: jiff::Timestamp::now(),
            title: None,
            model_provider: None,
            cli_version: None,
        })
        .await
        .unwrap_or_else(|err| panic!("create_process: {err}"));
    let lease = store
        .acquire_lease(&process_id, &"owner-1".to_string(), Duration::from_secs(30))
        .await
        .unwrap_or_else(|err| panic!("acquire_lease: {err}"));

    let err = match store
        .append_batch(AppendBatchInput {
            process_id,
            owner_id: "owner-1".to_string(),
            lease_token: lease.lease_token,
            expected_next_seq: 5,
            items: vec![JournalEntry {
                seq: 5,
                recorded_at: jiff::Timestamp::now(),
                item: RolloutItem::Compacted(CompactedItem {
                    message: "bad seq".to_string(),
                    replacement_history: None,
                }),
            }],
        })
        .await
    {
        Ok(_) => panic!("append_batch unexpectedly succeeded"),
        Err(err) => err,
    };

    match err {
        crate::error::JournalError::SequenceConflict { .. } => {}
        other => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn initialize_process_atomically_creates_row_lease_and_entries() {
    let temp_dir = tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));
    let db_path = temp_dir.path().join("journal.sqlite");
    let store = SqliteJournalStore::open(&db_path)
        .await
        .unwrap_or_else(|err| panic!("open: {err}"));

    let process_id = ProcessId::new();
    let created_at = jiff::Timestamp::now();
    let recorded_at = jiff::Timestamp::now();
    let item = RolloutItem::Compacted(CompactedItem {
        message: "first-batch".to_string(),
        replacement_history: None,
    });

    let result = store
        .initialize_process(InitializeProcessInput {
            create: CreateProcessInput {
                process_id,
                parent: None,
                source: SessionSource::Cli,
                cwd: temp_dir.path().to_path_buf(),
                created_at,
                title: Some("init-test".to_string()),
                model_provider: Some("openai".to_string()),
                cli_version: Some("47.0.0".to_string()),
            },
            owner_id: "owner-init".to_string(),
            ttl_ms: 30_000,
            items: vec![JournalEntry {
                seq: 0,
                recorded_at,
                item: item.clone(),
            }],
        })
        .await
        .unwrap_or_else(|err| panic!("initialize_process: {err}"));

    assert_eq!(result.process.process_id, process_id);
    assert_eq!(result.process.title, "init-test");
    assert_eq!(result.next_seq, 1);
    assert_eq!(result.lease.owner_id, "owner-init");
    assert!(!result.lease.lease_token.is_empty());

    // Process row visible.
    let stored = store
        .get_process(&process_id)
        .await
        .unwrap_or_else(|err| panic!("get_process: {err}"))
        .expect("process row missing");
    assert_eq!(stored.process_id, process_id);

    // First batch durably persisted at seq 0.
    let loaded = store
        .load_journal(&process_id)
        .await
        .unwrap_or_else(|err| panic!("load_journal: {err}"));
    assert_eq!(loaded.items.len(), 1);
    assert_eq!(loaded.items[0].seq, 0);
    assert_eq!(loaded.next_seq, 1);

    // The returned lease must be usable for a follow-on append at seq 1.
    store
        .append_batch(AppendBatchInput {
            process_id,
            owner_id: result.lease.owner_id.clone(),
            lease_token: result.lease.lease_token.clone(),
            expected_next_seq: 1,
            items: vec![JournalEntry {
                seq: 1,
                recorded_at: jiff::Timestamp::now(),
                item: RolloutItem::Compacted(CompactedItem {
                    message: "second".to_string(),
                    replacement_history: None,
                }),
            }],
        })
        .await
        .unwrap_or_else(|err| panic!("follow-on append_batch: {err}"));

    // Calling initialize again with the same process id must surface AlreadyExists.
    let dup_err = store
        .initialize_process(InitializeProcessInput {
            create: CreateProcessInput {
                process_id,
                parent: None,
                source: SessionSource::Cli,
                cwd: temp_dir.path().to_path_buf(),
                created_at,
                title: None,
                model_provider: None,
                cli_version: None,
            },
            owner_id: "owner-init".to_string(),
            ttl_ms: 30_000,
            items: vec![JournalEntry {
                seq: 0,
                recorded_at,
                item: item.clone(),
            }],
        })
        .await
        .expect_err("duplicate initialize_process should fail");
    match dup_err {
        crate::error::JournalError::ProcessAlreadyExists(returned) => {
            assert_eq!(returned, process_id);
        }
        other => panic!("expected ProcessAlreadyExists, got {other}"),
    }

    // Empty initial batch must be rejected (would otherwise create the very orphan row
    // this op exists to prevent).
    let empty_err = store
        .initialize_process(InitializeProcessInput {
            create: CreateProcessInput {
                process_id: ProcessId::new(),
                parent: None,
                source: SessionSource::Cli,
                cwd: temp_dir.path().to_path_buf(),
                created_at,
                title: None,
                model_provider: None,
                cli_version: None,
            },
            owner_id: "owner-init".to_string(),
            ttl_ms: 30_000,
            items: Vec::new(),
        })
        .await
        .expect_err("empty-batch initialize_process should fail");
    assert!(matches!(
        empty_err,
        crate::error::JournalError::InvalidRequest(_)
    ));
}
