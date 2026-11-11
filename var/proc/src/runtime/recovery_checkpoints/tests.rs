use super::*;
use crate::runtime::test_support::test_process_metadata;
use serde_json::json;
use std::path::Path;

pub(super) fn write(process_id: ProcessId, call_id: &str) -> RecoveryCheckpointWrite {
    RecoveryCheckpointWrite {
        process_id,
        recovery_id: None,
        call_id: call_id.into(),
        note: RecoveryCheckpointNote {
            summary: "Build paused before linking".into(),
            next_steps: vec!["Run the focused test".into()],
            blockers: vec![],
            artifacts: vec!["src/lib.rs".into()],
            running_tasks: vec!["exec:42 (stopped)".into()],
        },
        observation: json!({"phase": "warning", "generation": 1}),
    }
}

pub(super) async fn owner(db: &RuntimeDbHandle, home: &Path) -> anyhow::Result<ProcessId> {
    let id = ProcessId::new();
    let metadata = test_process_metadata(home, id, home.into());
    match db {
        RuntimeDbHandle::Sqlite(runtime) => runtime.upsert_process(&metadata).await?,
        RuntimeDbHandle::Postgres(runtime) => runtime.upsert_process(&metadata, None).await?,
    }
    Ok(id)
}

async fn fixture() -> anyhow::Result<(tempfile::TempDir, RuntimeDbHandle, ProcessId)> {
    let home = tempfile::tempdir()?;
    let db = open_database(home.path()).await?;
    let process = owner(&db, home.path()).await?;
    Ok((home, db, process))
}

async fn open_database(home: &Path) -> anyhow::Result<RuntimeDbHandle> {
    Ok(RuntimeDbHandle::Sqlite(
        StateRuntime::init(home.into(), "test".into()).await?,
    ))
}

pub(super) async fn contract(db: &RuntimeDbHandle, process: ProcessId) -> anyhow::Result<()> {
    let mut request = write(process, "same-call");
    let committed = db.save_recovery_checkpoint(&request).await?;
    assert_eq!(committed.new_recovery_id, Some(1));
    let saved = committed.checkpoint;
    let other = owner(db, db.chaos_home()).await?;
    let other_saved = db
        .save_recovery_checkpoint(&write(other, "same-call"))
        .await?
        .checkpoint;
    assert_eq!(saved.note, request.note);
    assert_eq!(saved.process_id, process);
    assert_eq!((saved.id, saved.recovery_id), (1, 1));
    assert_eq!(other_saved.id, saved.id, "IDs are scoped to their session");
    assert!(saved.created_at > 0);
    assert_eq!(
        db.recovery_checkpoint(process, None).await?,
        Some(saved.clone())
    );
    assert_eq!(
        db.recovery_checkpoint(process, Some(saved.id)).await?,
        Some(saved.clone())
    );
    assert!(
        db.recovery_checkpoint(ProcessId::new(), Some(saved.id))
            .await?
            .is_none()
    );
    request.observation = json!({"phase": "stabilizing"});
    let retry = db.save_recovery_checkpoint(&request).await?;
    assert_eq!(retry.checkpoint, saved, "retry retains original evidence");
    assert_eq!(retry.new_recovery_id, None);
    request.note.summary = "different".into();
    assert!(db.save_recovery_checkpoint(&request).await.is_err());
    assert_eq!(db.recovery_checkpoint_ids(process).await?, vec![saved.id]);

    let mut same_recovery = write(process, "same-recovery");
    same_recovery.recovery_id = Some(saved.recovery_id);
    let second = db.save_recovery_checkpoint(&same_recovery).await?;
    assert_eq!(second.new_recovery_id, None);
    assert_eq!(
        (second.checkpoint.id, second.checkpoint.recovery_id),
        (2, 1)
    );
    for n in 0..MAX_RECOVERY_CHECKPOINTS {
        db.save_recovery_checkpoint(&write(process, &format!("retained-{n}")))
            .await?;
    }
    assert_eq!(
        db.recovery_checkpoint_ids(process).await?.len(),
        MAX_RECOVERY_CHECKPOINTS as usize
    );
    let latest = db.recovery_checkpoint(process, None).await?.unwrap();
    assert_eq!(latest.id, MAX_RECOVERY_CHECKPOINTS + 2);
    assert_eq!(latest.recovery_id, MAX_RECOVERY_CHECKPOINTS + 1);
    assert!(
        db.recovery_checkpoint(process, Some(saved.id))
            .await?
            .is_none()
    );
    assert_eq!(
        db.recovery_checkpoint(other, None).await?,
        Some(other_saved)
    );
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoints_roundtrip_retry_isolation_and_retention() -> anyhow::Result<()> {
    let (_home, db, process) = fixture().await?;
    contract(&db, process).await
}

#[tokio::test]
async fn recovery_checkpoints_survive_database_reopen() -> anyhow::Result<()> {
    let (home, db, process) = fixture().await?;
    let saved = db
        .save_recovery_checkpoint(&write(process, "restart"))
        .await?
        .checkpoint;
    if let RuntimeDbHandle::Sqlite(runtime) = &db {
        runtime.pool().close().await;
    }
    drop(db);
    let reopened = open_database(home.path()).await?;
    assert_eq!(
        reopened.recovery_checkpoint(process, None).await?,
        Some(saved)
    );
    let next = reopened
        .save_recovery_checkpoint(&write(process, "after-restart"))
        .await?
        .checkpoint;
    assert_eq!((next.id, next.recovery_id), (2, 2));
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoint_order_does_not_depend_on_host_clock() -> anyhow::Result<()> {
    let (_home, db, process) = fixture().await?;
    let saved = db
        .save_recovery_checkpoint(&write(process, "ahead-clock"))
        .await?
        .checkpoint;
    if let RuntimeDbHandle::Sqlite(runtime) = &db {
        sqlx::query(
            "UPDATE recovery_checkpoints SET created_at = $1 WHERE process_id = $2 AND id = $3",
        )
        .bind(i64::MAX)
        .bind(process.to_string())
        .bind(saved.id)
        .execute(runtime.pool())
        .await?;
    }
    let next = db
        .save_recovery_checkpoint(&write(process, "after-resume"))
        .await?
        .checkpoint;
    assert_eq!(
        db.recovery_checkpoint(process, None).await?,
        Some(next.clone())
    );
    assert_eq!(db.recovery_checkpoint_ids(process).await?[0], next.id);
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoint_counters_survive_empty_history_and_restart() -> anyhow::Result<()> {
    let (home, db, process) = fixture().await?;
    let first = db
        .save_recovery_checkpoint(&write(process, "before-pruning"))
        .await?
        .checkpoint;
    if let RuntimeDbHandle::Sqlite(runtime) = &db {
        sqlx::query("DELETE FROM recovery_checkpoints WHERE process_id = $1")
            .bind(process.to_string())
            .execute(runtime.pool())
            .await?;
        runtime.pool().close().await;
    }
    drop(db);
    let reopened = open_database(home.path()).await?;
    assert!(reopened.recovery_checkpoint_ids(process).await?.is_empty());
    let next = reopened
        .save_recovery_checkpoint(&write(process, "after-pruning"))
        .await?
        .checkpoint;
    assert_eq!(next.id, first.id + 1);
    assert_eq!(next.recovery_id, first.recovery_id + 1);
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoints_reject_invalid_input_and_storage_failures() -> anyhow::Result<()> {
    let (_home, db, process) = fixture().await?;
    let mut request = write(process, "invalid");
    request.note.summary = " ".into();
    assert!(db.save_recovery_checkpoint(&request).await.is_err());
    request.note.summary = "é".repeat(MAX_RECOVERY_CHECKPOINT_BYTES / 2);
    assert!(
        db.save_recovery_checkpoint(&request).await.is_err(),
        "bound bytes, not characters"
    );
    request = write(process, "invalid");
    request.observation = json!("x".repeat(MAX_RECOVERY_CHECKPOINT_BYTES));
    assert!(db.save_recovery_checkpoint(&request).await.is_err());
    for recovery_id in [-1, 0, 1] {
        let mut request = write(process, "unissued-recovery");
        request.recovery_id = Some(recovery_id);
        assert!(db.save_recovery_checkpoint(&request).await.is_err());
    }
    assert!(
        db.save_recovery_checkpoint(&write(ProcessId::new(), "missing-owner"))
            .await
            .is_err()
    );
    assert!(db.recovery_checkpoint_ids(process).await?.is_empty());
    if let RuntimeDbHandle::Sqlite(runtime) = &db {
        sqlx::query(
            "CREATE TRIGGER checkpoint_failure BEFORE INSERT ON recovery_checkpoints \
                     BEGIN SELECT RAISE(ABORT, 'simulated storage failure'); END",
        )
        .execute(runtime.pool())
        .await?;
    }
    assert!(
        db.save_recovery_checkpoint(&write(process, "read-only"))
            .await
            .is_err()
    );
    assert!(db.recovery_checkpoint_ids(process).await?.is_empty());
    if let RuntimeDbHandle::Sqlite(runtime) = &db {
        sqlx::query("DROP TRIGGER checkpoint_failure")
            .execute(runtime.pool())
            .await?;
    }
    let saved = db
        .save_recovery_checkpoint(&write(process, "storage-restored"))
        .await?
        .checkpoint;
    assert_eq!(
        (saved.id, saved.recovery_id),
        (1, 1),
        "failed saves roll back counters"
    );
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoint_sequences_do_not_wrap() -> anyhow::Result<()> {
    let (_home, db, process) = fixture().await?;
    db.save_recovery_checkpoint(&write(process, "first"))
        .await?;
    if let RuntimeDbHandle::Sqlite(runtime) = &db {
        for (last_checkpoint, last_recovery) in [(i64::MAX, 1), (1, i64::MAX)] {
            sqlx::query(
                "UPDATE recovery_checkpoint_counters \
                 SET last_checkpoint_id = $2, last_recovery_id = $3 WHERE process_id = $1",
            )
            .bind(process.to_string())
            .bind(last_checkpoint)
            .bind(last_recovery)
            .execute(runtime.pool())
            .await?;
            assert!(
                db.save_recovery_checkpoint(&write(process, "overflow"))
                    .await
                    .is_err()
            );
            assert_eq!(db.recovery_checkpoint_ids(process).await?, vec![1]);
        }
    }
    Ok(())
}

pub(super) async fn concurrent_saves(
    db: &RuntimeDbHandle,
    process: ProcessId,
) -> anyhow::Result<()> {
    let first = write(process, "concurrent");
    let second = write(process, "concurrent");
    let (a, b) = tokio::join!(
        db.save_recovery_checkpoint(&first),
        db.save_recovery_checkpoint(&second)
    );
    let (a, b) = (a?, b?);
    assert_eq!(a.checkpoint, b.checkpoint);
    assert_eq!(
        [a.new_recovery_id, b.new_recovery_id]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(db.recovery_checkpoint_ids(process).await?.len(), 1);
    let mut first = write(process, "concurrent-a");
    let mut second = write(process, "concurrent-b");
    first.recovery_id = Some(1);
    second.recovery_id = Some(1);
    let (a, b) = tokio::join!(
        db.save_recovery_checkpoint(&first),
        db.save_recovery_checkpoint(&second)
    );
    let (a, b) = (a?.checkpoint, b?.checkpoint);
    assert_ne!(a.id, b.id);
    assert_eq!((a.recovery_id, b.recovery_id), (1, 1));
    assert_eq!(db.recovery_checkpoint_ids(process).await?, vec![3, 2, 1]);
    Ok(())
}

#[tokio::test]
async fn recovery_checkpoints_serialize_concurrent_retries() -> anyhow::Result<()> {
    let (_home, db, process) = fixture().await?;
    concurrent_saves(&db, process).await
}
