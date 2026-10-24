use super::*;
use std::path::PathBuf;

#[tokio::test]
async fn progress_decodes_empty_and_populated_aggregates_for_only_the_requested_job() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("open SQLite");
    sqlx::query("CREATE TABLE agent_job_items (job_id TEXT, status TEXT)")
        .execute(&pool)
        .await
        .expect("create table");
    let runtime = StateRuntime::from_sqlite_pool(PathBuf::new(), "test".into(), pool.clone());
    let empty = runtime
        .get_minion_job_progress("job")
        .await
        .expect("empty progress");
    assert_eq!(
        empty,
        MinionJobProgress {
            total_items: 0,
            pending_items: 0,
            running_items: 0,
            completed_items: 0,
            failed_items: 0,
        }
    );

    sqlx::query(
        "INSERT INTO agent_job_items VALUES
         ('job', 'pending'), ('job', 'pending'), ('job', 'running'),
         ('job', 'completed'), ('job', 'failed'), ('other', 'failed')",
    )
    .execute(&pool)
    .await
    .expect("insert statuses");
    let progress = runtime
        .get_minion_job_progress("job")
        .await
        .expect("job progress");
    assert_eq!(
        progress,
        MinionJobProgress {
            total_items: 5,
            pending_items: 2,
            running_items: 1,
            completed_items: 1,
            failed_items: 1,
        }
    );
}
