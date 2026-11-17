use super::*;
use crate::execution::replay::{MAX_EVENTS, MAX_REPLAY_BYTES};

async fn append(store: &SqliteJournalStore, lease: &ExecutionLease, after: u64, body: &str) {
    let response = store
        .execution(ExecutionRequest::Replay {
            request: ExecutionReplayRequest::Append {
                lease: lease.clone(),
                after,
                event: json!({ "body": body }),
            },
        })
        .await
        .unwrap();
    assert!(matches!(
        response,
        ExecutionResponse::Replay(ExecutionReplayResponse::Appended { sequence })
            if sequence == after + 1
    ));
}

async fn read(
    store: &SqliteJournalStore,
    lease: &ExecutionLease,
    after: u64,
) -> Result<ExecutionReplayBatch, JournalError> {
    match store
        .execution(ExecutionRequest::Replay {
            request: ExecutionReplayRequest::Read {
                session_id: lease.session_id,
                after,
            },
        })
        .await?
    {
        ExecutionResponse::Replay(ExecutionReplayResponse::Batch(batch)) => Ok(batch),
        other => panic!("expected replay batch: {other:?}"),
    }
}

async fn stored(store: &SqliteJournalStore, lease: &ExecutionLease) -> Vec<(i64, i64)> {
    sqlx::query_as(
        "SELECT sequence, event_bytes FROM execution_replay_events
        WHERE session_id = $1 ORDER BY sequence",
    )
    .bind(lease.session_id.to_string())
    .fetch_all(store.pool())
    .await
    .unwrap()
}

async fn sqlite() -> (tempfile::TempDir, SqliteJournalStore) {
    let home = tempfile::tempdir().unwrap();
    let store = SqliteJournalStore::open(&home.path().join("journal.sqlite"))
        .await
        .unwrap();
    (home, store)
}

#[tokio::test]
async fn count_eviction_deletes_exactly_the_evicted_range_and_reports_coverage() {
    let (_home, store) = sqlite().await;
    let lease = bound(&store).await;
    let empty = read(&store, &lease, 0).await.unwrap();
    assert!(!empty.gap);
    assert_eq!(empty.next_cursor, 0);
    assert!(read(&store, &lease, 1).await.is_err());
    let total = MAX_EVENTS as u64 + 7;
    for after in 0..total {
        append(&store, &lease, after, "small").await;
    }
    let rows = stored(&store, &lease).await;
    let floor = total - MAX_EVENTS as u64 + 1;
    assert_eq!(rows.len(), MAX_EVENTS);
    assert_eq!(rows.first().unwrap().0 as u64, floor);
    assert_eq!(rows.last().unwrap().0 as u64, total);
    assert!(rows.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1));

    let behind = read(&store, &lease, floor - 2).await.unwrap();
    assert!(behind.gap);
    assert_eq!(behind.events.len(), MAX_EVENTS);
    let covered = read(&store, &lease, floor - 1).await.unwrap();
    assert!(!covered.gap);
    assert_eq!(covered.events[0].sequence, floor);
    let tail = read(&store, &lease, total).await.unwrap();
    assert!(!tail.gap && tail.events.is_empty());
    assert_eq!(tail.next_cursor, total);
    assert!(matches!(
        read(&store, &lease, total + 1).await,
        Err(JournalError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn byte_eviction_keeps_the_newest_records_within_budget() {
    let (_home, store) = sqlite().await;
    let lease = bound(&store).await;
    let body = "a".repeat(60 * 1024);
    for after in 0..20 {
        append(&store, &lease, after, &body).await;
    }
    let rows = stored(&store, &lease).await;
    let bytes: i64 = rows.iter().map(|row| row.1).sum();
    assert!(rows.len() < 20);
    assert!(bytes as u64 <= MAX_REPLAY_BYTES.get());
    assert!((bytes + rows[0].1) as u64 > MAX_REPLAY_BYTES.get());
    assert_eq!(rows.last().unwrap().0, 20);
    let floor = rows[0].0 as u64;
    assert!(read(&store, &lease, 0).await.unwrap().gap);
    assert!(!read(&store, &lease, floor - 1).await.unwrap().gap);
}

#[tokio::test]
async fn a_window_no_appends_explain_refuses_to_restore() {
    let (_home, store) = sqlite().await;
    let lease = bound(&store).await;
    for after in 0..3 {
        append(&store, &lease, after, "small").await;
    }
    sqlx::query("DELETE FROM execution_replay_events WHERE session_id = $1 AND sequence = 2")
        .bind(lease.session_id.to_string())
        .execute(store.pool())
        .await
        .unwrap();
    assert!(matches!(
        read(&store, &lease, 0).await,
        Err(JournalError::ExecutionConflict(_))
    ));
    let refused = store
        .execution(ExecutionRequest::Replay {
            request: ExecutionReplayRequest::Append {
                lease: lease.clone(),
                after: 3,
                event: json!({ "body": "next" }),
            },
        })
        .await;
    assert!(matches!(refused, Err(JournalError::ExecutionConflict(_))));
    assert_eq!(stored(&store, &lease).await.len(), 2);
}
