use super::StateRuntime;
use crate::runtime::test_support::unique_temp_dir;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn init_creates_runtime_db() {
    let chaos_home = unique_temp_dir();
    tokio::fs::create_dir_all(&chaos_home)
        .await
        .expect("create chaos_home");

    let _runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    assert_eq!(
        tokio::fs::try_exists(chaos_home.join(crate::runtime::runtime_db_filename()))
            .await
            .expect("check new db path"),
        true
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn backfill_state_persists_progress_and_completion() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let initial = runtime
        .backfill()
        .get_state()
        .await
        .expect("get initial backfill state");
    assert_eq!(initial.status, crate::BackfillStatus::Pending);
    assert_eq!(initial.last_watermark, None);
    assert_eq!(initial.last_success_at, None);

    runtime
        .backfill()
        .mark_running()
        .await
        .expect("mark backfill running");
    runtime
        .backfill()
        .checkpoint("cursor-a")
        .await
        .expect("checkpoint backfill");

    let running = runtime
        .backfill()
        .get_state()
        .await
        .expect("get running backfill state");
    assert_eq!(running.status, crate::BackfillStatus::Running);
    assert_eq!(running.last_watermark, Some("cursor-a".to_string()));
    assert_eq!(running.last_success_at, None);

    runtime
        .backfill()
        .mark_complete(Some("cursor-b"))
        .await
        .expect("mark backfill complete");
    let completed = runtime
        .backfill()
        .get_state()
        .await
        .expect("get completed backfill state");
    assert_eq!(completed.status, crate::BackfillStatus::Complete);
    assert_eq!(completed.last_watermark, Some("cursor-b".to_string()));
    assert!(completed.last_success_at.is_some());

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn backfill_claim_is_singleton_until_stale_and_blocked_when_complete() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let claimed = runtime
        .backfill()
        .try_claim(3600)
        .await
        .expect("initial backfill claim");
    assert_eq!(claimed, true);

    let duplicate_claim = runtime
        .backfill()
        .try_claim(3600)
        .await
        .expect("duplicate backfill claim");
    assert_eq!(duplicate_claim, false);

    let stale_updated_at = jiff::Timestamp::now().as_second().saturating_sub(10_000);
    sqlx::query(
        r#"
UPDATE backfill_state
SET status = ?, updated_at = ?
WHERE id = 1
            "#,
    )
    .bind(crate::BackfillStatus::Running.as_str())
    .bind(stale_updated_at)
    .execute(runtime.pool.as_ref())
    .await
    .expect("force stale backfill lease");

    let stale_claim = runtime
        .backfill()
        .try_claim(10)
        .await
        .expect("stale backfill claim");
    assert_eq!(stale_claim, true);

    runtime
        .backfill()
        .mark_complete(None)
        .await
        .expect("mark complete");
    let claim_after_complete = runtime
        .backfill()
        .try_claim(3600)
        .await
        .expect("claim after complete");
    assert_eq!(claim_after_complete, false);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn mark_backfill_running_is_idempotent_after_claim() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let claimed = runtime
        .backfill()
        .try_claim(3600)
        .await
        .expect("claim backfill");
    assert_eq!(claimed, true);

    runtime
        .backfill()
        .mark_running()
        .await
        .expect("mark running after claim");

    let state = runtime
        .backfill()
        .get_state()
        .await
        .expect("get backfill state after claim");
    assert_eq!(state.status, crate::BackfillStatus::Running);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}
