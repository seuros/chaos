use super::*;
use crate::model::ParentRef;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::SubAgentSource;

fn entry(seq: i64) -> JournalEntry {
    JournalEntry {
        seq,
        recorded_at: jiff::Timestamp::UNIX_EPOCH,
        item: RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: Vec::new(),
            end_turn: None,
            phase: None,
        }),
    }
}

#[test]
fn sequence_validation_handles_initial_and_appended_batches() {
    let process_id = ProcessId::new();
    for first_seq in [0, 9] {
        assert!(validate_sequences(process_id, first_seq, &[]).is_ok());
        assert!(
            validate_sequences(
                process_id,
                first_seq,
                &[entry(first_seq), entry(first_seq + 1)]
            )
            .is_ok()
        );
        match validate_sequences(
            process_id,
            first_seq,
            &[entry(first_seq), entry(first_seq + 2)],
        ) {
            Err(JournalError::SequenceConflict {
                process_id: actual_process,
                expected_next_seq,
                actual_next_seq,
            }) => {
                assert_eq!(actual_process, process_id);
                assert_eq!(expected_next_seq, first_seq + 1);
                assert_eq!(actual_next_seq, first_seq + 2);
            }
            other => panic!("expected the first sequence mismatch, got {other:?}"),
        }
    }
}

#[test]
fn lease_timestamps_preserve_second_precision_and_reject_overflow() {
    let base = jiff::Timestamp::from_second(42).expect("valid timestamp");
    assert_eq!(
        timestamp_after(base, Duration::from_millis(1999))
            .expect("valid expiration")
            .as_second(),
        43
    );
    assert!(timestamp_after(base, Duration::from_secs(u64::MAX)).is_err());
    assert!(timestamp_from_epoch_seconds(i64::MAX).is_err());
}

#[test]
fn new_records_default_metadata_and_preserve_explicit_values() {
    let mut input = CreateProcessInput {
        process_id: ProcessId::new(),
        parent: None,
        source: SessionSource::Cli,
        cwd: "/workspace".into(),
        created_at: jiff::Timestamp::UNIX_EPOCH,
        title: None,
        model_provider: None,
        cli_version: None,
    };
    let record = new_process_record(input.clone(), input.created_at);
    assert_eq!(record.title, "");
    assert_eq!(record.model_provider, "unknown");
    assert_eq!(record.cli_version, None);
    assert_eq!(record.agent_nickname, None);
    assert_eq!(record.agent_role, None);
    assert_eq!(record.updated_at, input.created_at);
    assert_eq!(record.archived_at, None);

    let parent_process_id = ProcessId::new();
    input.parent = Some(ParentRef {
        parent_process_id,
        fork_at_seq: 7,
    });
    input.source = SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
        parent_process_id,
        depth: 1,
        agent_nickname: Some("worker".into()),
        agent_role: Some("reviewer".into()),
    });
    input.title = Some("review".into());
    // An explicitly empty provider must not be replaced by the default.
    input.model_provider = Some(String::new());
    input.cli_version = Some("47.0.0".into());
    let updated_at = timestamp_from_epoch_seconds(42).expect("valid timestamp");
    let record = new_process_record(input.clone(), updated_at);
    assert_eq!(record.process_id, input.process_id);
    assert_eq!(record.parent, input.parent);
    assert_eq!(record.source, input.source);
    assert_eq!(record.cwd, input.cwd);
    assert_eq!(record.created_at, input.created_at);
    assert_eq!(record.updated_at, updated_at);
    assert_eq!(record.archived_at, None);
    assert_eq!(record.title, "review");
    assert_eq!(record.model_provider, "");
    assert_eq!(record.cli_version, input.cli_version);
    assert_eq!(record.agent_nickname.as_deref(), Some("worker"));
    assert_eq!(record.agent_role.as_deref(), Some("reviewer"));
}

#[tokio::test]
async fn process_insert_classifies_only_unique_violations() {
    let process_id = ProcessId::new();
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("open SQLite");
    sqlx::query("CREATE TABLE processes (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .expect("create table");
    let error = sqlx::query("INSERT INTO processes VALUES (1), (1)")
        .execute(&pool)
        .await
        .expect_err("duplicate key");
    std::assert_matches!(
        process_insert_error(process_id, error),
        JournalError::ProcessAlreadyExists(id) if id == process_id
    );

    let error = sqlx::query("SELECT missing FROM processes")
        .execute(&pool)
        .await
        .expect_err("missing column");
    std::assert_matches!(
        process_insert_error(process_id, error),
        JournalError::Db(sqlx::Error::Database(_))
    );
    std::assert_matches!(
        process_insert_error(process_id, sqlx::Error::PoolTimedOut),
        JournalError::Db(sqlx::Error::PoolTimedOut)
    );
}
