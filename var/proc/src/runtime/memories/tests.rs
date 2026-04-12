use super::super::StateRuntime;
use super::JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL;
use super::JOB_KIND_MEMORY_STAGE1;
use super::whole_days_as_hours;
use crate::model::Phase2JobClaimOutcome;
use crate::model::Stage1JobClaimOutcome;
use crate::model::Stage1StartupClaimParams;
use crate::runtime::test_support::test_process_metadata;
use crate::runtime::test_support::unique_temp_dir;
use chaos_ipc::ProcessId;
use jiff::ToSpan;
use pretty_assertions::assert_eq;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;
#[tokio::test]
async fn stage1_claim_skips_when_up_to_date() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let metadata = test_process_metadata(&chaos_home, process_id, chaos_home.join("a"));
    runtime
        .upsert_process(&metadata)
        .await
        .expect("upsert thread");

    let owner_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    let claim = runtime
        .try_claim_stage1_job(process_id, owner_a, 100, 3600, 64)
        .await
        .expect("claim stage1 job");
    let ownership_token = match claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected claim outcome: {other:?}"),
    };

    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                ownership_token.as_str(),
                100,
                "raw",
                "sum",
                None,
            )
            .await
            .expect("mark stage1 succeeded"),
        "stage1 success should finalize for current token"
    );

    let up_to_date = runtime
        .try_claim_stage1_job(process_id, owner_b, 100, 3600, 64)
        .await
        .expect("claim stage1 up-to-date");
    assert_eq!(up_to_date, Stage1JobClaimOutcome::SkippedUpToDate);

    let needs_rerun = runtime
        .try_claim_stage1_job(process_id, owner_b, 101, 3600, 64)
        .await
        .expect("claim stage1 newer source");
    assert!(
        matches!(needs_rerun, Stage1JobClaimOutcome::Claimed { .. }),
        "newer source_updated_at should be claimable"
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn stage1_running_stale_can_be_stolen_but_fresh_running_is_skipped() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let cwd = chaos_home.join("workspace");
    runtime
        .upsert_process(&test_process_metadata(&chaos_home, process_id, cwd))
        .await
        .expect("upsert thread");

    let claim_a = runtime
        .try_claim_stage1_job(process_id, owner_a, 100, 3600, 64)
        .await
        .expect("claim a");
    assert!(matches!(claim_a, Stage1JobClaimOutcome::Claimed { .. }));

    let claim_b_fresh = runtime
        .try_claim_stage1_job(process_id, owner_b, 100, 3600, 64)
        .await
        .expect("claim b fresh");
    assert_eq!(claim_b_fresh, Stage1JobClaimOutcome::SkippedRunning);

    sqlx::query("UPDATE jobs SET lease_until = 0 WHERE kind = 'memory_stage1' AND job_key = ?")
        .bind(process_id.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("force stale lease");

    let claim_b_stale = runtime
        .try_claim_stage1_job(process_id, owner_b, 100, 3600, 64)
        .await
        .expect("claim b stale");
    assert!(matches!(
        claim_b_stale,
        Stage1JobClaimOutcome::Claimed { .. }
    ));

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn stage1_concurrent_claim_for_same_thread_is_conflict_safe() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    let owner_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let process_id_a = process_id;
    let process_id_b = process_id;
    let runtime_a = Arc::clone(&runtime);
    let runtime_b = Arc::clone(&runtime);
    let claim_with_retry = |runtime: Arc<StateRuntime>, process_id: ProcessId, owner: ProcessId| async move {
        for attempt in 0..5 {
            match runtime
                .try_claim_stage1_job(process_id, owner, 100, 3_600, 64)
                .await
            {
                Ok(outcome) => return outcome,
                Err(err) if err.to_string().contains("database is locked") && attempt < 4 => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(err) => panic!("claim stage1 should not fail: {err}"),
            }
        }
        panic!("claim stage1 should have returned within retry budget")
    };

    let (claim_a, claim_b) = tokio::join!(
        claim_with_retry(runtime_a, process_id_a, owner_a),
        claim_with_retry(runtime_b, process_id_b, owner_b),
    );

    let claim_outcomes = vec![claim_a, claim_b];
    let claimed_count = claim_outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Stage1JobClaimOutcome::Claimed { .. }))
        .count();
    assert_eq!(claimed_count, 1);
    assert!(
        claim_outcomes.iter().all(|outcome| {
            matches!(
                outcome,
                Stage1JobClaimOutcome::Claimed { .. } | Stage1JobClaimOutcome::SkippedRunning
            )
        }),
        "unexpected claim outcomes: {claim_outcomes:?}"
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn stage1_concurrent_claims_respect_running_cap() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let thread_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let thread_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            thread_a,
            chaos_home.join("workspace-a"),
        ))
        .await
        .expect("upsert thread a");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            thread_b,
            chaos_home.join("workspace-b"),
        ))
        .await
        .expect("upsert thread b");

    let owner_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let runtime_a = Arc::clone(&runtime);
    let runtime_b = Arc::clone(&runtime);

    let (claim_a, claim_b) = tokio::join!(
        async move {
            runtime_a
                .try_claim_stage1_job(thread_a, owner_a, 100, 3_600, 1)
                .await
                .expect("claim stage1 thread a")
        },
        async move {
            runtime_b
                .try_claim_stage1_job(thread_b, owner_b, 101, 3_600, 1)
                .await
                .expect("claim stage1 thread b")
        },
    );

    let claim_outcomes = vec![claim_a, claim_b];
    let claimed_count = claim_outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Stage1JobClaimOutcome::Claimed { .. }))
        .count();
    assert_eq!(claimed_count, 1);
    assert!(
        claim_outcomes
            .iter()
            .any(|outcome| { matches!(outcome, Stage1JobClaimOutcome::SkippedRunning) }),
        "one concurrent claim should be throttled by running cap: {claim_outcomes:?}"
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn claim_stage1_jobs_filters_by_age_idle_and_current_process() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let now = jiff::Timestamp::now();
    let fresh_at = now.checked_sub(1.hours()).unwrap();
    let just_under_idle_at = now
        .checked_sub(12.hours())
        .unwrap()
        .checked_add(1.minutes())
        .unwrap();
    let eligible_idle_at = now
        .checked_sub(12.hours())
        .unwrap()
        .checked_sub(1.minutes())
        .unwrap();
    let old_at = now.checked_sub(whole_days_as_hours(31)).unwrap();

    let current_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("current thread id");
    let fresh_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("fresh thread id");
    let just_under_idle_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("just under idle thread id");
    let eligible_idle_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("eligible idle thread id");
    let old_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("old thread id");

    let mut current =
        test_process_metadata(&chaos_home, current_process_id, chaos_home.join("current"));
    current.created_at = now;
    current.updated_at = now;
    runtime
        .upsert_process(&current)
        .await
        .expect("upsert current");

    let mut fresh = test_process_metadata(&chaos_home, fresh_process_id, chaos_home.join("fresh"));
    fresh.created_at = fresh_at;
    fresh.updated_at = fresh_at;
    runtime.upsert_process(&fresh).await.expect("upsert fresh");

    let mut just_under_idle = test_process_metadata(
        &chaos_home,
        just_under_idle_process_id,
        chaos_home.join("just-under-idle"),
    );
    just_under_idle.created_at = just_under_idle_at;
    just_under_idle.updated_at = just_under_idle_at;
    runtime
        .upsert_process(&just_under_idle)
        .await
        .expect("upsert just-under-idle");

    let mut eligible_idle = test_process_metadata(
        &chaos_home,
        eligible_idle_process_id,
        chaos_home.join("eligible-idle"),
    );
    eligible_idle.created_at = eligible_idle_at;
    eligible_idle.updated_at = eligible_idle_at;
    runtime
        .upsert_process(&eligible_idle)
        .await
        .expect("upsert eligible-idle");

    let mut old = test_process_metadata(&chaos_home, old_process_id, chaos_home.join("old"));
    old.created_at = old_at;
    old.updated_at = old_at;
    runtime.upsert_process(&old).await.expect("upsert old");

    let allowed_sources = vec!["cli".to_string()];
    let claims = runtime
        .claim_stage1_jobs_for_startup(
            current_process_id,
            Stage1StartupClaimParams {
                scan_limit: 1,
                max_claimed: 5,
                max_age_days: 30,
                min_rollout_idle_hours: 12,
                allowed_sources: allowed_sources.as_slice(),
                lease_seconds: 3600,
            },
        )
        .await
        .expect("claim stage1 jobs");

    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].thread.id, eligible_idle_process_id);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn claim_stage1_jobs_prefilters_threads_with_up_to_date_memory() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let now = jiff::Timestamp::now();
    let eligible_newer_at = now.checked_sub(13.hours()).unwrap();
    let eligible_older_at = now.checked_sub(14.hours()).unwrap();

    let current_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("current thread id");
    let up_to_date_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("up-to-date thread id");
    let stale_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("stale thread id");
    let worker_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("worker id");

    let mut current =
        test_process_metadata(&chaos_home, current_process_id, chaos_home.join("current"));
    current.created_at = now;
    current.updated_at = now;
    runtime
        .upsert_process(&current)
        .await
        .expect("upsert current thread");

    let mut up_to_date = test_process_metadata(
        &chaos_home,
        up_to_date_process_id,
        chaos_home.join("up-to-date"),
    );
    up_to_date.created_at = eligible_newer_at;
    up_to_date.updated_at = eligible_newer_at;
    runtime
        .upsert_process(&up_to_date)
        .await
        .expect("upsert up-to-date thread");

    let up_to_date_claim = runtime
        .try_claim_stage1_job(
            up_to_date_process_id,
            worker_id,
            up_to_date.updated_at.as_second(),
            3600,
            64,
        )
        .await
        .expect("claim up-to-date thread for seed");
    let up_to_date_token = match up_to_date_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected seed claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                up_to_date_process_id,
                up_to_date_token.as_str(),
                up_to_date.updated_at.as_second(),
                "raw",
                "summary",
                None,
            )
            .await
            .expect("mark up-to-date thread succeeded"),
        "seed stage1 success should complete for up-to-date thread"
    );

    let mut stale = test_process_metadata(&chaos_home, stale_process_id, chaos_home.join("stale"));
    stale.created_at = eligible_older_at;
    stale.updated_at = eligible_older_at;
    runtime
        .upsert_process(&stale)
        .await
        .expect("upsert stale thread");

    let allowed_sources = vec!["cli".to_string()];
    let claims = runtime
        .claim_stage1_jobs_for_startup(
            current_process_id,
            Stage1StartupClaimParams {
                scan_limit: 1,
                max_claimed: 1,
                max_age_days: 30,
                min_rollout_idle_hours: 12,
                allowed_sources: allowed_sources.as_slice(),
                lease_seconds: 3600,
            },
        )
        .await
        .expect("claim stage1 startup jobs");
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].thread.id, stale_process_id);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn claim_stage1_jobs_skips_threads_with_disabled_memory_mode() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let now = jiff::Timestamp::now();
    let eligible_at = now.checked_sub(13.hours()).unwrap();

    let current_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("current thread id");
    let disabled_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("disabled thread id");
    let enabled_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("enabled thread id");

    let mut current =
        test_process_metadata(&chaos_home, current_process_id, chaos_home.join("current"));
    current.created_at = now;
    current.updated_at = now;
    runtime
        .upsert_process(&current)
        .await
        .expect("upsert current thread");

    let mut disabled = test_process_metadata(
        &chaos_home,
        disabled_process_id,
        chaos_home.join("disabled"),
    );
    disabled.created_at = eligible_at;
    disabled.updated_at = eligible_at;
    runtime
        .upsert_process(&disabled)
        .await
        .expect("upsert disabled thread");
    sqlx::query("UPDATE processes SET memory_mode = 'disabled' WHERE id = ?")
        .bind(disabled_process_id.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("disable thread memory mode");

    let mut enabled =
        test_process_metadata(&chaos_home, enabled_process_id, chaos_home.join("enabled"));
    enabled.created_at = eligible_at;
    enabled.updated_at = eligible_at;
    runtime
        .upsert_process(&enabled)
        .await
        .expect("upsert enabled thread");

    let allowed_sources = vec!["cli".to_string()];
    let claims = runtime
        .claim_stage1_jobs_for_startup(
            current_process_id,
            Stage1StartupClaimParams {
                scan_limit: 10,
                max_claimed: 10,
                max_age_days: 30,
                min_rollout_idle_hours: 12,
                allowed_sources: allowed_sources.as_slice(),
                lease_seconds: 3600,
            },
        )
        .await
        .expect("claim stage1 startup jobs");

    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].thread.id, enabled_process_id);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn reset_memory_data_for_fresh_start_clears_rows_and_disables_processes() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let now = jiff::Timestamp::now().checked_sub(13.hours()).unwrap();
    let worker_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("worker id");
    let enabled_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("enabled thread id");
    let disabled_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("disabled thread id");

    let mut enabled =
        test_process_metadata(&chaos_home, enabled_process_id, chaos_home.join("enabled"));
    enabled.created_at = now;
    enabled.updated_at = now;
    runtime
        .upsert_process(&enabled)
        .await
        .expect("upsert enabled thread");

    let claim = runtime
        .try_claim_stage1_job(
            enabled_process_id,
            worker_id,
            enabled.updated_at.as_second(),
            3600,
            64,
        )
        .await
        .expect("claim enabled thread");
    let ownership_token = match claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                enabled_process_id,
                ownership_token.as_str(),
                enabled.updated_at.as_second(),
                "raw",
                "summary",
                None,
            )
            .await
            .expect("mark enabled thread succeeded"),
        "stage1 success should be recorded"
    );
    runtime
        .enqueue_global_consolidation(enabled.updated_at.as_second())
        .await
        .expect("enqueue global consolidation");

    let mut disabled = test_process_metadata(
        &chaos_home,
        disabled_process_id,
        chaos_home.join("disabled"),
    );
    disabled.created_at = now;
    disabled.updated_at = now;
    runtime
        .upsert_process(&disabled)
        .await
        .expect("upsert disabled thread");
    sqlx::query("UPDATE processes SET memory_mode = 'disabled' WHERE id = ?")
        .bind(disabled_process_id.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("disable existing process");

    runtime
        .reset_memory_data_for_fresh_start()
        .await
        .expect("reset memory data");

    let stage1_outputs_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stage1_outputs")
        .fetch_one(runtime.pool.as_ref())
        .await
        .expect("count stage1 outputs");
    assert_eq!(stage1_outputs_count, 0);

    let memory_jobs_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE kind = ? OR kind = ?")
            .bind(JOB_KIND_MEMORY_STAGE1)
            .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("count memory jobs");
    assert_eq!(memory_jobs_count, 0);

    let enabled_memory_mode: String =
        sqlx::query_scalar("SELECT memory_mode FROM processes WHERE id = ?")
            .bind(enabled_process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("read enabled thread memory mode");
    assert_eq!(enabled_memory_mode, "disabled");

    let disabled_memory_mode: String =
        sqlx::query_scalar("SELECT memory_mode FROM processes WHERE id = ?")
            .bind(disabled_process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("read disabled thread memory mode");
    assert_eq!(disabled_memory_mode, "disabled");

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn claim_stage1_jobs_enforces_global_running_cap() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let current_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("current thread id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            current_process_id,
            chaos_home.join("current"),
        ))
        .await
        .expect("upsert current");

    let now = jiff::Timestamp::now();
    let started_at = now.as_second();
    let lease_until = started_at + 3600;
    let eligible_at = now.checked_sub(13.hours()).unwrap();
    let existing_running = 10usize;
    let total_candidates = 80usize;

    for idx in 0..total_candidates {
        let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
        let mut metadata = test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join(format!("thread-{idx}")),
        );
        metadata.created_at = eligible_at.checked_sub((idx as i64).seconds()).unwrap();
        metadata.updated_at = eligible_at.checked_sub((idx as i64).seconds()).unwrap();
        runtime
            .upsert_process(&metadata)
            .await
            .expect("upsert thread");

        if idx < existing_running {
            sqlx::query(
                r#"
INSERT INTO jobs (
    kind,
    job_key,
    status,
    worker_id,
    ownership_token,
    started_at,
    finished_at,
    lease_until,
    retry_at,
    retry_remaining,
    last_error,
    input_watermark,
    last_success_watermark
) VALUES (?, ?, 'running', ?, ?, ?, NULL, ?, NULL, ?, NULL, ?, NULL)
                    "#,
            )
            .bind("memory_stage1")
            .bind(process_id.to_string())
            .bind(current_process_id.to_string())
            .bind(Uuid::new_v4().to_string())
            .bind(started_at)
            .bind(lease_until)
            .bind(3)
            .bind(metadata.updated_at.as_second())
            .execute(runtime.pool.as_ref())
            .await
            .expect("seed running stage1 job");
        }
    }

    let allowed_sources = vec!["cli".to_string()];
    let claims = runtime
        .claim_stage1_jobs_for_startup(
            current_process_id,
            Stage1StartupClaimParams {
                scan_limit: 200,
                max_claimed: 64,
                max_age_days: 30,
                min_rollout_idle_hours: 12,
                allowed_sources: allowed_sources.as_slice(),
                lease_seconds: 3600,
            },
        )
        .await
        .expect("claim stage1 jobs");
    assert_eq!(claims.len(), 54);

    let running_count = sqlx::query(
        r#"
SELECT COUNT(*) AS count
FROM jobs
WHERE kind = 'memory_stage1'
  AND status = 'running'
  AND lease_until IS NOT NULL
  AND lease_until > ?
            "#,
    )
    .bind(jiff::Timestamp::now().as_second())
    .fetch_one(runtime.pool.as_ref())
    .await
    .expect("count running stage1 jobs")
    .try_get::<i64, _>("count")
    .expect("running count value");
    assert_eq!(running_count, 64);

    let more_claims = runtime
        .claim_stage1_jobs_for_startup(
            current_process_id,
            Stage1StartupClaimParams {
                scan_limit: 200,
                max_claimed: 64,
                max_age_days: 30,
                min_rollout_idle_hours: 12,
                allowed_sources: allowed_sources.as_slice(),
                lease_seconds: 3600,
            },
        )
        .await
        .expect("claim stage1 jobs with cap reached");
    assert_eq!(more_claims.len(), 0);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn claim_stage1_jobs_processes_two_full_batches_across_startup_passes() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let current_process_id =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("current thread id");
    let mut current =
        test_process_metadata(&chaos_home, current_process_id, chaos_home.join("current"));
    current.created_at = jiff::Timestamp::now();
    current.updated_at = jiff::Timestamp::now();
    runtime
        .upsert_process(&current)
        .await
        .expect("upsert current");

    let eligible_at = jiff::Timestamp::now().checked_sub(13.hours()).unwrap();
    for idx in 0..200 {
        let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
        let mut metadata = test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join(format!("thread-{idx}")),
        );
        metadata.created_at = eligible_at.checked_sub((idx as i64).seconds()).unwrap();
        metadata.updated_at = eligible_at.checked_sub((idx as i64).seconds()).unwrap();
        runtime
            .upsert_process(&metadata)
            .await
            .expect("upsert eligible thread");
    }

    let allowed_sources = vec!["cli".to_string()];
    let first_claims = runtime
        .claim_stage1_jobs_for_startup(
            current_process_id,
            Stage1StartupClaimParams {
                scan_limit: 5_000,
                max_claimed: 64,
                max_age_days: 30,
                min_rollout_idle_hours: 12,
                allowed_sources: allowed_sources.as_slice(),
                lease_seconds: 3_600,
            },
        )
        .await
        .expect("first stage1 startup claim");
    assert_eq!(first_claims.len(), 64);

    for claim in first_claims {
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    claim.thread.id,
                    claim.ownership_token.as_str(),
                    claim.thread.updated_at.as_second(),
                    "raw",
                    "summary",
                    None,
                )
                .await
                .expect("mark first-batch stage1 success"),
            "first batch stage1 completion should succeed"
        );
    }

    let second_claims = runtime
        .claim_stage1_jobs_for_startup(
            current_process_id,
            Stage1StartupClaimParams {
                scan_limit: 5_000,
                max_claimed: 64,
                max_age_days: 30,
                min_rollout_idle_hours: 12,
                allowed_sources: allowed_sources.as_slice(),
                lease_seconds: 3_600,
            },
        )
        .await
        .expect("second stage1 startup claim");
    assert_eq!(second_claims.len(), 64);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn stage1_output_cascades_on_thread_delete() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let cwd = chaos_home.join("workspace");
    runtime
        .upsert_process(&test_process_metadata(&chaos_home, process_id, cwd))
        .await
        .expect("upsert thread");

    let claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
        .await
        .expect("claim stage1");
    let ownership_token = match claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                ownership_token.as_str(),
                100,
                "raw",
                "sum",
                None,
            )
            .await
            .expect("mark stage1 succeeded"),
        "mark stage1 succeeded should write stage1_outputs"
    );

    let count_before =
        sqlx::query("SELECT COUNT(*) AS count FROM stage1_outputs WHERE process_id = ?")
            .bind(process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("count before delete")
            .try_get::<i64, _>("count")
            .expect("count value");
    assert_eq!(count_before, 1);

    sqlx::query("DELETE FROM processes WHERE id = ?")
        .bind(process_id.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("delete thread");

    let count_after =
        sqlx::query("SELECT COUNT(*) AS count FROM stage1_outputs WHERE process_id = ?")
            .bind(process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("count after delete")
            .try_get::<i64, _>("count")
            .expect("count value");
    assert_eq!(count_after, 0);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn mark_stage1_job_succeeded_no_output_skips_phase2_when_output_was_already_absent() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    let claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
        .await
        .expect("claim stage1");
    let ownership_token = match claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded_no_output(process_id, ownership_token.as_str())
            .await
            .expect("mark stage1 succeeded without output"),
        "stage1 no-output success should complete the job"
    );

    let output_row_count =
        sqlx::query("SELECT COUNT(*) AS count FROM stage1_outputs WHERE process_id = ?")
            .bind(process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("load stage1 output count")
            .try_get::<i64, _>("count")
            .expect("stage1 output count");
    assert_eq!(
        output_row_count, 0,
        "stage1 no-output success should not persist empty stage1 outputs"
    );

    let up_to_date = runtime
        .try_claim_stage1_job(process_id, owner_b, 100, 3600, 64)
        .await
        .expect("claim stage1 up-to-date");
    assert_eq!(up_to_date, Stage1JobClaimOutcome::SkippedUpToDate);

    let global_job_row_count = sqlx::query("SELECT COUNT(*) AS count FROM jobs WHERE kind = ?")
        .bind("memory_consolidate_global")
        .fetch_one(runtime.pool.as_ref())
        .await
        .expect("load phase2 job row count")
        .try_get::<i64, _>("count")
        .expect("phase2 job row count");
    assert_eq!(
        global_job_row_count, 0,
        "no-output without an existing stage1 output should not enqueue phase2"
    );

    let claim_phase2 = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    assert_eq!(
        claim_phase2,
        Phase2JobClaimOutcome::SkippedNotDirty,
        "phase2 should remain clean when no-output deleted nothing"
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn mark_stage1_job_succeeded_no_output_enqueues_phase2_when_deleting_output() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    let first_claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
        .await
        .expect("claim initial stage1");
    let first_token = match first_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected initial stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(process_id, first_token.as_str(), 100, "raw", "sum", None)
            .await
            .expect("mark initial stage1 succeeded"),
        "initial stage1 success should create stage1 output"
    );

    let phase2_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2 after initial output");
    let (phase2_token, phase2_input_watermark) = match phase2_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim after initial output: {other:?}"),
    };
    assert_eq!(phase2_input_watermark, 100);
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(phase2_token.as_str(), phase2_input_watermark, &[],)
            .await
            .expect("mark initial phase2 succeeded"),
        "initial phase2 success should clear global dirty state"
    );

    let no_output_claim = runtime
        .try_claim_stage1_job(process_id, owner_b, 101, 3600, 64)
        .await
        .expect("claim stage1 for no-output delete");
    let no_output_token = match no_output_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected no-output stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded_no_output(process_id, no_output_token.as_str())
            .await
            .expect("mark stage1 no-output after existing output"),
        "no-output should succeed when deleting an existing stage1 output"
    );

    let output_row_count =
        sqlx::query("SELECT COUNT(*) AS count FROM stage1_outputs WHERE process_id = ?")
            .bind(process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("load stage1 output count after delete")
            .try_get::<i64, _>("count")
            .expect("stage1 output count");
    assert_eq!(output_row_count, 0);

    let claim_phase2 = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2 after no-output deletion");
    let (phase2_token, phase2_input_watermark) = match claim_phase2 {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim after no-output deletion: {other:?}"),
    };
    assert_eq!(phase2_input_watermark, 101);
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(phase2_token.as_str(), phase2_input_watermark, &[],)
            .await
            .expect("mark phase2 succeeded after no-output delete")
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn stage1_retry_exhaustion_does_not_block_newer_watermark() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    for attempt in 0..3 {
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, 100, 3_600, 64)
            .await
            .expect("claim stage1 for retry exhaustion");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!(
                "attempt {} should claim stage1 before retries are exhausted: {other:?}",
                attempt + 1
            ),
        };
        assert!(
            runtime
                .mark_stage1_job_failed(process_id, ownership_token.as_str(), "boom", 0)
                .await
                .expect("mark stage1 failed"),
            "attempt {} should decrement retry budget",
            attempt + 1
        );
    }

    let exhausted_claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3_600, 64)
        .await
        .expect("claim stage1 after retry exhaustion");
    assert_eq!(
        exhausted_claim,
        Stage1JobClaimOutcome::SkippedRetryExhausted
    );

    let newer_source_claim = runtime
        .try_claim_stage1_job(process_id, owner, 101, 3_600, 64)
        .await
        .expect("claim stage1 with newer source watermark");
    assert!(
        matches!(newer_source_claim, Stage1JobClaimOutcome::Claimed { .. }),
        "newer source watermark should reset retry budget and be claimable"
    );

    let job_row = sqlx::query(
        "SELECT retry_remaining, input_watermark FROM jobs WHERE kind = ? AND job_key = ?",
    )
    .bind("memory_stage1")
    .bind(process_id.to_string())
    .fetch_one(runtime.pool.as_ref())
    .await
    .expect("load stage1 job row after newer-source claim");
    assert_eq!(
        job_row
            .try_get::<i64, _>("retry_remaining")
            .expect("retry_remaining"),
        3
    );
    assert_eq!(
        job_row
            .try_get::<i64, _>("input_watermark")
            .expect("input_watermark"),
        101
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn phase2_global_consolidation_reruns_when_watermark_advances() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    runtime
        .enqueue_global_consolidation(100)
        .await
        .expect("enqueue global consolidation");

    let claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    let (ownership_token, input_watermark) = match claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(ownership_token.as_str(), input_watermark, &[],)
            .await
            .expect("mark phase2 succeeded"),
        "phase2 success should finalize for current token"
    );

    let claim_up_to_date = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2 up-to-date");
    assert_eq!(claim_up_to_date, Phase2JobClaimOutcome::SkippedNotDirty);

    runtime
        .enqueue_global_consolidation(101)
        .await
        .expect("enqueue global consolidation again");

    let claim_rerun = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2 rerun");
    assert!(
        matches!(claim_rerun, Phase2JobClaimOutcome::Claimed { .. }),
        "advanced watermark should be claimable"
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn list_stage1_outputs_for_global_returns_latest_outputs() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let process_id_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id_a,
            chaos_home.join("workspace-a"),
        ))
        .await
        .expect("upsert thread a");
    let mut metadata_b =
        test_process_metadata(&chaos_home, process_id_b, chaos_home.join("workspace-b"));
    metadata_b.git_branch = Some("feature/stage1-b".to_string());
    runtime
        .upsert_process(&metadata_b)
        .await
        .expect("upsert thread b");

    let claim = runtime
        .try_claim_stage1_job(process_id_a, owner, 100, 3600, 64)
        .await
        .expect("claim stage1 a");
    let ownership_token = match claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id_a,
                ownership_token.as_str(),
                100,
                "raw memory a",
                "summary a",
                None,
            )
            .await
            .expect("mark stage1 succeeded a"),
        "stage1 success should persist output a"
    );

    let claim = runtime
        .try_claim_stage1_job(process_id_b, owner, 101, 3600, 64)
        .await
        .expect("claim stage1 b");
    let ownership_token = match claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id_b,
                ownership_token.as_str(),
                101,
                "raw memory b",
                "summary b",
                Some("rollout-b"),
            )
            .await
            .expect("mark stage1 succeeded b"),
        "stage1 success should persist output b"
    );

    let outputs = runtime
        .list_stage1_outputs_for_global(10)
        .await
        .expect("list stage1 outputs for global");
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].process_id, process_id_b);
    assert_eq!(outputs[0].rollout_summary, "summary b");
    assert_eq!(outputs[0].rollout_slug.as_deref(), Some("rollout-b"));
    assert_eq!(outputs[0].cwd, chaos_home.join("workspace-b"));
    assert_eq!(outputs[0].git_branch.as_deref(), Some("feature/stage1-b"));
    assert_eq!(outputs[1].process_id, process_id_a);
    assert_eq!(outputs[1].rollout_summary, "summary a");
    assert_eq!(outputs[1].rollout_slug, None);
    assert_eq!(outputs[1].cwd, chaos_home.join("workspace-a"));
    assert_eq!(outputs[1].git_branch, None);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn list_stage1_outputs_for_global_skips_empty_payloads() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id_non_empty =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let process_id_empty = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id_non_empty,
            chaos_home.join("workspace-non-empty"),
        ))
        .await
        .expect("upsert non-empty thread");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id_empty,
            chaos_home.join("workspace-empty"),
        ))
        .await
        .expect("upsert empty thread");

    sqlx::query(
            r#"
INSERT INTO stage1_outputs (process_id, source_updated_at, raw_memory, rollout_summary, generated_at)
VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(process_id_non_empty.to_string())
        .bind(100_i64)
        .bind("raw memory")
        .bind("summary")
        .bind(100_i64)
        .execute(runtime.pool.as_ref())
        .await
        .expect("insert non-empty stage1 output");
    sqlx::query(
            r#"
INSERT INTO stage1_outputs (process_id, source_updated_at, raw_memory, rollout_summary, generated_at)
VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(process_id_empty.to_string())
        .bind(101_i64)
        .bind("")
        .bind("")
        .bind(101_i64)
        .execute(runtime.pool.as_ref())
        .await
        .expect("insert empty stage1 output");

    let outputs = runtime
        .list_stage1_outputs_for_global(1)
        .await
        .expect("list stage1 outputs for global");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].process_id, process_id_non_empty);
    assert_eq!(outputs[0].rollout_summary, "summary");
    assert_eq!(outputs[0].cwd, chaos_home.join("workspace-non-empty"));

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn list_stage1_outputs_for_global_skips_polluted_processes() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id_enabled =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let process_id_polluted =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    for (process_id, workspace) in [
        (process_id_enabled, "workspace-enabled"),
        (process_id_polluted, "workspace-polluted"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");

        let claim = runtime
            .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    100,
                    "raw memory",
                    "summary",
                    None,
                )
                .await
                .expect("mark stage1 succeeded"),
            "stage1 success should persist output"
        );
    }

    runtime
        .set_process_memory_mode(process_id_polluted, "polluted")
        .await
        .expect("mark thread polluted");

    let outputs = runtime
        .list_stage1_outputs_for_global(10)
        .await
        .expect("list stage1 outputs for global");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].process_id, process_id_enabled);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn get_phase2_input_selection_reports_added_retained_and_removed_rows() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let process_id_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let process_id_c = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    for (process_id, workspace) in [
        (process_id_a, "workspace-a"),
        (process_id_b, "workspace-b"),
        (process_id_c, "workspace-c"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");
    }

    for (process_id, updated_at, slug) in [
        (process_id_a, 100, Some("rollout-a")),
        (process_id_b, 101, Some("rollout-b")),
        (process_id_c, 102, Some("rollout-c")),
    ] {
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, updated_at, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    updated_at,
                    &format!("raw-{updated_at}"),
                    &format!("summary-{updated_at}"),
                    slug,
                )
                .await
                .expect("mark stage1 succeeded"),
            "stage1 success should persist output"
        );
    }

    let claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    let (ownership_token, input_watermark) = match claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim outcome: {other:?}"),
    };
    assert_eq!(input_watermark, 102);
    let selected_outputs = runtime
        .list_stage1_outputs_for_global(10)
        .await
        .expect("list stage1 outputs for global")
        .into_iter()
        .filter(|output| output.process_id == process_id_c || output.process_id == process_id_a)
        .collect::<Vec<_>>();
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                ownership_token.as_str(),
                input_watermark,
                &selected_outputs,
            )
            .await
            .expect("mark phase2 success with selection"),
        "phase2 success should persist selected rows"
    );

    let selection = runtime
        .get_phase2_input_selection(2, 36_500)
        .await
        .expect("load phase2 input selection");

    assert_eq!(selection.selected.len(), 2);
    assert_eq!(selection.previous_selected.len(), 2);
    assert_eq!(selection.selected[0].process_id, process_id_c);
    assert_eq!(selection.selected[0].process_ref, process_id_c.to_string());
    assert_eq!(selection.selected[1].process_id, process_id_b);
    assert_eq!(selection.retained_process_ids, vec![process_id_c]);

    assert_eq!(selection.removed.len(), 1);
    assert_eq!(selection.removed[0].process_id, process_id_a);
    assert_eq!(
        selection.removed[0].rollout_slug.as_deref(),
        Some("rollout-a")
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn get_phase2_input_selection_marks_polluted_previous_selection_as_removed() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id_enabled =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let process_id_polluted =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    for (process_id, updated_at) in [(process_id_enabled, 100), (process_id_polluted, 101)] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(process_id.to_string()),
            ))
            .await
            .expect("upsert thread");

        let claim = runtime
            .try_claim_stage1_job(process_id, owner, updated_at, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    updated_at,
                    &format!("raw-{updated_at}"),
                    &format!("summary-{updated_at}"),
                    None,
                )
                .await
                .expect("mark stage1 succeeded"),
            "stage1 success should persist output"
        );
    }

    let claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    let (ownership_token, input_watermark) = match claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim outcome: {other:?}"),
    };
    let selected_outputs = runtime
        .list_stage1_outputs_for_global(10)
        .await
        .expect("list stage1 outputs for global");
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                ownership_token.as_str(),
                input_watermark,
                &selected_outputs,
            )
            .await
            .expect("mark phase2 success"),
        "phase2 success should persist selected rows"
    );

    runtime
        .set_process_memory_mode(process_id_polluted, "polluted")
        .await
        .expect("mark thread polluted");

    let selection = runtime
        .get_phase2_input_selection(2, 36_500)
        .await
        .expect("load phase2 input selection");

    assert_eq!(selection.selected.len(), 1);
    assert_eq!(selection.selected[0].process_id, process_id_enabled);
    assert_eq!(selection.previous_selected.len(), 2);
    assert!(
        selection
            .previous_selected
            .iter()
            .any(|item| item.process_id == process_id_enabled)
    );
    assert!(
        selection
            .previous_selected
            .iter()
            .any(|item| item.process_id == process_id_polluted)
    );
    assert_eq!(selection.retained_process_ids, vec![process_id_enabled]);
    assert_eq!(selection.removed.len(), 1);
    assert_eq!(selection.removed[0].process_id, process_id_polluted);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn mark_process_memory_mode_polluted_enqueues_phase2_for_selected_processes() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    let claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
        .await
        .expect("claim stage1");
    let ownership_token = match claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                ownership_token.as_str(),
                100,
                "raw",
                "summary",
                None,
            )
            .await
            .expect("mark stage1 succeeded"),
        "stage1 success should persist output"
    );

    let phase2_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    let (phase2_token, input_watermark) = match phase2_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim outcome: {other:?}"),
    };
    let selected_outputs = runtime
        .list_stage1_outputs_for_global(10)
        .await
        .expect("list stage1 outputs");
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                phase2_token.as_str(),
                input_watermark,
                &selected_outputs,
            )
            .await
            .expect("mark phase2 success"),
        "phase2 success should persist selected rows"
    );

    assert!(
        runtime
            .mark_process_memory_mode_polluted(process_id)
            .await
            .expect("mark thread polluted"),
        "thread should transition to polluted"
    );

    let next_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2 after pollution");
    assert!(matches!(next_claim, Phase2JobClaimOutcome::Claimed { .. }));

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn get_phase2_input_selection_treats_regenerated_selected_rows_as_added() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    let first_claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
        .await
        .expect("claim initial stage1");
    let first_token = match first_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                first_token.as_str(),
                100,
                "raw-100",
                "summary-100",
                Some("rollout-100"),
            )
            .await
            .expect("mark initial stage1 success"),
        "initial stage1 success should persist output"
    );

    let phase2_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    let (phase2_token, input_watermark) = match phase2_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim outcome: {other:?}"),
    };
    let selected_outputs = runtime
        .list_stage1_outputs_for_global(1)
        .await
        .expect("list selected outputs");
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                phase2_token.as_str(),
                input_watermark,
                &selected_outputs,
            )
            .await
            .expect("mark phase2 success"),
        "phase2 success should persist selected rows"
    );

    let refreshed_claim = runtime
        .try_claim_stage1_job(process_id, owner, 101, 3600, 64)
        .await
        .expect("claim refreshed stage1");
    let refreshed_token = match refreshed_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                refreshed_token.as_str(),
                101,
                "raw-101",
                "summary-101",
                Some("rollout-101"),
            )
            .await
            .expect("mark refreshed stage1 success"),
        "refreshed stage1 success should persist output"
    );

    let selection = runtime
        .get_phase2_input_selection(1, 36_500)
        .await
        .expect("load phase2 input selection");
    assert_eq!(selection.selected.len(), 1);
    assert_eq!(selection.previous_selected.len(), 1);
    assert_eq!(selection.selected[0].process_id, process_id);
    assert_eq!(selection.selected[0].source_updated_at.as_second(), 101);
    assert!(selection.retained_process_ids.is_empty());
    assert!(selection.removed.is_empty());

    let (selected_for_phase2, selected_for_phase2_source_updated_at) =
            sqlx::query_as::<_, (i64, Option<i64>)>(
                "SELECT selected_for_phase2, selected_for_phase2_source_updated_at FROM stage1_outputs WHERE process_id = ?",
            )
        .bind(process_id.to_string())
        .fetch_one(runtime.pool.as_ref())
        .await
        .expect("load selected_for_phase2");
    assert_eq!(selected_for_phase2, 1);
    assert_eq!(selected_for_phase2_source_updated_at, Some(100));

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn get_phase2_input_selection_reports_regenerated_previous_selection_as_removed() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread a");
    let process_id_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread b");
    let process_id_c = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread c");
    let process_id_d = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread d");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    for (process_id, workspace) in [
        (process_id_a, "workspace-a"),
        (process_id_b, "workspace-b"),
        (process_id_c, "workspace-c"),
        (process_id_d, "workspace-d"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");
    }

    for (process_id, updated_at, slug) in [
        (process_id_a, 100, Some("rollout-a-100")),
        (process_id_b, 101, Some("rollout-b-101")),
        (process_id_c, 99, Some("rollout-c-99")),
        (process_id_d, 98, Some("rollout-d-98")),
    ] {
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, updated_at, 3600, 64)
            .await
            .expect("claim initial stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    updated_at,
                    &format!("raw-{updated_at}"),
                    &format!("summary-{updated_at}"),
                    slug,
                )
                .await
                .expect("mark stage1 succeeded"),
            "stage1 success should persist output"
        );
    }

    let phase2_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    let (phase2_token, input_watermark) = match phase2_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim outcome: {other:?}"),
    };
    let selected_outputs = runtime
        .list_stage1_outputs_for_global(2)
        .await
        .expect("list selected outputs");
    assert_eq!(
        selected_outputs
            .iter()
            .map(|output| output.process_id)
            .collect::<Vec<_>>(),
        vec![process_id_b, process_id_a]
    );
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                phase2_token.as_str(),
                input_watermark,
                &selected_outputs,
            )
            .await
            .expect("mark phase2 success"),
        "phase2 success should persist selected rows"
    );

    for (process_id, updated_at, slug) in [
        (process_id_a, 102, Some("rollout-a-102")),
        (process_id_c, 103, Some("rollout-c-103")),
        (process_id_d, 104, Some("rollout-d-104")),
    ] {
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, updated_at, 3600, 64)
            .await
            .expect("claim refreshed stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    updated_at,
                    &format!("raw-{updated_at}"),
                    &format!("summary-{updated_at}"),
                    slug,
                )
                .await
                .expect("mark refreshed stage1 success"),
            "refreshed stage1 success should persist output"
        );
    }

    let selection = runtime
        .get_phase2_input_selection(2, 36_500)
        .await
        .expect("load phase2 input selection");
    assert_eq!(
        selection
            .selected
            .iter()
            .map(|output| output.process_id)
            .collect::<Vec<_>>(),
        vec![process_id_d, process_id_c]
    );
    assert_eq!(
        selection
            .previous_selected
            .iter()
            .map(|output| output.process_id)
            .collect::<Vec<_>>(),
        vec![process_id_a, process_id_b]
    );
    assert!(selection.retained_process_ids.is_empty());
    assert_eq!(
        selection
            .removed
            .iter()
            .map(|output| (output.process_id, output.source_updated_at.as_second()))
            .collect::<Vec<_>>(),
        vec![(process_id_a, 102), (process_id_b, 101)]
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn mark_global_phase2_job_succeeded_updates_selected_snapshot_timestamp() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    let initial_claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
        .await
        .expect("claim initial stage1");
    let initial_token = match initial_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                initial_token.as_str(),
                100,
                "raw-100",
                "summary-100",
                Some("rollout-100"),
            )
            .await
            .expect("mark initial stage1 success"),
        "initial stage1 success should persist output"
    );

    let first_phase2_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim first phase2");
    let (first_phase2_token, first_input_watermark) = match first_phase2_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected first phase2 claim outcome: {other:?}"),
    };
    let first_selected_outputs = runtime
        .list_stage1_outputs_for_global(1)
        .await
        .expect("list first selected outputs");
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                first_phase2_token.as_str(),
                first_input_watermark,
                &first_selected_outputs,
            )
            .await
            .expect("mark first phase2 success"),
        "first phase2 success should persist selected rows"
    );

    let refreshed_claim = runtime
        .try_claim_stage1_job(process_id, owner, 101, 3600, 64)
        .await
        .expect("claim refreshed stage1");
    let refreshed_token = match refreshed_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected refreshed stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                refreshed_token.as_str(),
                101,
                "raw-101",
                "summary-101",
                Some("rollout-101"),
            )
            .await
            .expect("mark refreshed stage1 success"),
        "refreshed stage1 success should persist output"
    );

    let second_phase2_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim second phase2");
    let (second_phase2_token, second_input_watermark) = match second_phase2_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected second phase2 claim outcome: {other:?}"),
    };
    let second_selected_outputs = runtime
        .list_stage1_outputs_for_global(1)
        .await
        .expect("list second selected outputs");
    assert_eq!(
        second_selected_outputs[0].source_updated_at.as_second(),
        101
    );
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                second_phase2_token.as_str(),
                second_input_watermark,
                &second_selected_outputs,
            )
            .await
            .expect("mark second phase2 success"),
        "second phase2 success should persist selected rows"
    );

    let selection = runtime
        .get_phase2_input_selection(1, 36_500)
        .await
        .expect("load phase2 input selection after refresh");
    assert_eq!(selection.retained_process_ids, vec![process_id]);

    let (selected_for_phase2, selected_for_phase2_source_updated_at) =
            sqlx::query_as::<_, (i64, Option<i64>)>(
                "SELECT selected_for_phase2, selected_for_phase2_source_updated_at FROM stage1_outputs WHERE process_id = ?",
            )
            .bind(process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("load selected snapshot after phase2");
    assert_eq!(selected_for_phase2, 1);
    assert_eq!(selected_for_phase2_source_updated_at, Some(101));

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn mark_global_phase2_job_succeeded_only_marks_exact_selected_snapshots() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let process_id = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            process_id,
            chaos_home.join("workspace"),
        ))
        .await
        .expect("upsert thread");

    let initial_claim = runtime
        .try_claim_stage1_job(process_id, owner, 100, 3600, 64)
        .await
        .expect("claim initial stage1");
    let initial_token = match initial_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                initial_token.as_str(),
                100,
                "raw-100",
                "summary-100",
                Some("rollout-100"),
            )
            .await
            .expect("mark initial stage1 success"),
        "initial stage1 success should persist output"
    );

    let phase2_claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim phase2");
    let (phase2_token, input_watermark) = match phase2_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected phase2 claim outcome: {other:?}"),
    };
    let selected_outputs = runtime
        .list_stage1_outputs_for_global(1)
        .await
        .expect("list selected outputs");
    assert_eq!(selected_outputs[0].source_updated_at.as_second(), 100);

    let refreshed_claim = runtime
        .try_claim_stage1_job(process_id, owner, 101, 3600, 64)
        .await
        .expect("claim refreshed stage1");
    let refreshed_token = match refreshed_claim {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(
                process_id,
                refreshed_token.as_str(),
                101,
                "raw-101",
                "summary-101",
                Some("rollout-101"),
            )
            .await
            .expect("mark refreshed stage1 success"),
        "refreshed stage1 success should persist output"
    );

    assert!(
        runtime
            .mark_global_phase2_job_succeeded(
                phase2_token.as_str(),
                input_watermark,
                &selected_outputs,
            )
            .await
            .expect("mark phase2 success"),
        "phase2 success should still complete"
    );

    let (selected_for_phase2, selected_for_phase2_source_updated_at) =
            sqlx::query_as::<_, (i64, Option<i64>)>(
                "SELECT selected_for_phase2, selected_for_phase2_source_updated_at FROM stage1_outputs WHERE process_id = ?",
            )
            .bind(process_id.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("load selected_for_phase2");
    assert_eq!(selected_for_phase2, 0);
    assert_eq!(selected_for_phase2_source_updated_at, None);

    let selection = runtime
        .get_phase2_input_selection(1, 36_500)
        .await
        .expect("load phase2 input selection");
    assert_eq!(selection.selected.len(), 1);
    assert_eq!(selection.selected[0].source_updated_at.as_second(), 101);
    assert!(selection.retained_process_ids.is_empty());

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn record_stage1_output_usage_updates_usage_metadata() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let thread_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id a");
    let thread_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id b");
    let missing = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("missing id");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            thread_a,
            chaos_home.join("workspace-a"),
        ))
        .await
        .expect("upsert thread a");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            thread_b,
            chaos_home.join("workspace-b"),
        ))
        .await
        .expect("upsert thread b");

    let claim_a = runtime
        .try_claim_stage1_job(thread_a, owner, 100, 3600, 64)
        .await
        .expect("claim stage1 a");
    let token_a = match claim_a {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome for a: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(thread_a, token_a.as_str(), 100, "raw a", "sum a", None)
            .await
            .expect("mark stage1 succeeded a")
    );

    let claim_b = runtime
        .try_claim_stage1_job(thread_b, owner, 101, 3600, 64)
        .await
        .expect("claim stage1 b");
    let token_b = match claim_b {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome for b: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(thread_b, token_b.as_str(), 101, "raw b", "sum b", None)
            .await
            .expect("mark stage1 succeeded b")
    );

    let updated_rows = runtime
        .record_stage1_output_usage(&[thread_a, thread_a, thread_b, missing])
        .await
        .expect("record stage1 output usage");
    assert_eq!(updated_rows, 3);

    let row_a =
        sqlx::query("SELECT usage_count, last_usage FROM stage1_outputs WHERE process_id = ?")
            .bind(thread_a.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("load stage1 usage row a");
    let row_b =
        sqlx::query("SELECT usage_count, last_usage FROM stage1_outputs WHERE process_id = ?")
            .bind(thread_b.to_string())
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("load stage1 usage row b");

    assert_eq!(
        row_a
            .try_get::<i64, _>("usage_count")
            .expect("usage_count a"),
        2
    );
    assert_eq!(
        row_b
            .try_get::<i64, _>("usage_count")
            .expect("usage_count b"),
        1
    );

    let last_usage_a = row_a.try_get::<i64, _>("last_usage").expect("last_usage a");
    let last_usage_b = row_b.try_get::<i64, _>("last_usage").expect("last_usage b");
    assert_eq!(last_usage_a, last_usage_b);
    assert!(last_usage_a > 0);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn get_phase2_input_selection_prioritizes_usage_count_then_recent_usage() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let now = jiff::Timestamp::now();
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let thread_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id a");
    let thread_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id b");
    let thread_c = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id c");

    for (process_id, workspace) in [
        (thread_a, "workspace-a"),
        (thread_b, "workspace-b"),
        (thread_c, "workspace-c"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");
    }

    for (process_id, generated_at, summary) in [
        (
            thread_a,
            now.checked_sub(whole_days_as_hours(3)).unwrap(),
            "summary-a",
        ),
        (
            thread_b,
            now.checked_sub(whole_days_as_hours(2)).unwrap(),
            "summary-b",
        ),
        (
            thread_c,
            now.checked_sub(whole_days_as_hours(1)).unwrap(),
            "summary-c",
        ),
    ] {
        let source_updated_at = generated_at.as_second();
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, source_updated_at, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    source_updated_at,
                    &format!("raw-{summary}"),
                    summary,
                    None,
                )
                .await
                .expect("mark stage1 success"),
            "stage1 success should persist output"
        );
    }

    for (process_id, usage_count, last_usage) in [
        (
            thread_a,
            5_i64,
            now.checked_sub(whole_days_as_hours(10)).unwrap(),
        ),
        (
            thread_b,
            5_i64,
            now.checked_sub(whole_days_as_hours(1)).unwrap(),
        ),
        (thread_c, 1_i64, now.checked_sub(1.hours()).unwrap()),
    ] {
        sqlx::query(
            "UPDATE stage1_outputs SET usage_count = ?, last_usage = ? WHERE process_id = ?",
        )
        .bind(usage_count)
        .bind(last_usage.as_second())
        .bind(process_id.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("update usage metadata");
    }

    let selection = runtime
        .get_phase2_input_selection(3, 30)
        .await
        .expect("load phase2 input selection");

    assert_eq!(
        selection
            .selected
            .iter()
            .map(|output| output.process_id)
            .collect::<Vec<_>>(),
        vec![thread_b, thread_a, thread_c]
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn get_phase2_input_selection_excludes_stale_used_memories_but_keeps_fresh_never_used() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let now = jiff::Timestamp::now();
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let thread_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id a");
    let thread_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id b");
    let thread_c = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id c");

    for (process_id, workspace) in [
        (thread_a, "workspace-a"),
        (thread_b, "workspace-b"),
        (thread_c, "workspace-c"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");
    }

    for (process_id, generated_at, summary) in [
        (
            thread_a,
            now.checked_sub(whole_days_as_hours(40)).unwrap(),
            "summary-a",
        ),
        (
            thread_b,
            now.checked_sub(whole_days_as_hours(2)).unwrap(),
            "summary-b",
        ),
        (
            thread_c,
            now.checked_sub(whole_days_as_hours(50)).unwrap(),
            "summary-c",
        ),
    ] {
        let source_updated_at = generated_at.as_second();
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, source_updated_at, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    source_updated_at,
                    &format!("raw-{summary}"),
                    summary,
                    None,
                )
                .await
                .expect("mark stage1 success"),
            "stage1 success should persist output"
        );
    }

    for (process_id, usage_count, last_usage) in [
        (
            thread_a,
            Some(9_i64),
            Some(now.checked_sub(whole_days_as_hours(31)).unwrap()),
        ),
        (thread_b, None, None),
        (
            thread_c,
            Some(1_i64),
            Some(now.checked_sub(whole_days_as_hours(1)).unwrap()),
        ),
    ] {
        sqlx::query(
            "UPDATE stage1_outputs SET usage_count = ?, last_usage = ? WHERE process_id = ?",
        )
        .bind(usage_count)
        .bind(last_usage.map(jiff::Timestamp::as_second))
        .bind(process_id.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("update usage metadata");
    }

    let selection = runtime
        .get_phase2_input_selection(3, 30)
        .await
        .expect("load phase2 input selection");

    assert_eq!(
        selection
            .selected
            .iter()
            .map(|output| output.process_id)
            .collect::<Vec<_>>(),
        vec![thread_c, thread_b]
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn get_phase2_input_selection_prefers_recent_thread_updates_over_recent_generation() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let older_thread =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("older thread id");
    let newer_thread =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("newer thread id");

    for (process_id, workspace) in [
        (older_thread, "workspace-older"),
        (newer_thread, "workspace-newer"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");
    }

    for (process_id, source_updated_at, summary) in [
        (older_thread, 100_i64, "summary-older"),
        (newer_thread, 200_i64, "summary-newer"),
    ] {
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, source_updated_at, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    source_updated_at,
                    &format!("raw-{summary}"),
                    summary,
                    None,
                )
                .await
                .expect("mark stage1 success"),
            "stage1 success should persist output"
        );
    }

    sqlx::query("UPDATE stage1_outputs SET generated_at = ? WHERE process_id = ?")
        .bind(300_i64)
        .bind(older_thread.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("update older generated_at");
    sqlx::query("UPDATE stage1_outputs SET generated_at = ? WHERE process_id = ?")
        .bind(150_i64)
        .bind(newer_thread.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("update newer generated_at");

    let selection = runtime
        .get_phase2_input_selection(1, 36_500)
        .await
        .expect("load phase2 input selection");

    assert_eq!(selection.selected.len(), 1);
    assert_eq!(selection.selected[0].process_id, newer_thread);
    assert_eq!(selection.selected[0].source_updated_at.as_second(), 200);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn prune_stage1_outputs_for_retention_prunes_stale_unselected_rows_only() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let stale_unused = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("stale unused");
    let stale_used = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("stale used");
    let stale_selected =
        ProcessId::from_string(&Uuid::new_v4().to_string()).expect("stale selected");
    let fresh_used = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("fresh used");

    for (process_id, workspace) in [
        (stale_unused, "workspace-stale-unused"),
        (stale_used, "workspace-stale-used"),
        (stale_selected, "workspace-stale-selected"),
        (fresh_used, "workspace-fresh-used"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");
    }

    let now = jiff::Timestamp::now().as_second();
    for (process_id, source_updated_at, summary) in [
        (stale_unused, now - 60 * 86400, "stale-unused"),
        (stale_used, now - 50 * 86400, "stale-used"),
        (stale_selected, now - 45 * 86400, "stale-selected"),
        (fresh_used, now - 10 * 86400, "fresh-used"),
    ] {
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, source_updated_at, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    source_updated_at,
                    &format!("raw-{summary}"),
                    summary,
                    None,
                )
                .await
                .expect("mark stage1 success"),
            "stage1 success should persist output"
        );
    }

    sqlx::query("UPDATE stage1_outputs SET usage_count = ?, last_usage = ? WHERE process_id = ?")
        .bind(3_i64)
        .bind(now - 40 * 86400)
        .bind(stale_used.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("set stale used metadata");
    sqlx::query(
            "UPDATE stage1_outputs SET selected_for_phase2 = 1, selected_for_phase2_source_updated_at = source_updated_at WHERE process_id = ?",
        )
        .bind(stale_selected.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("mark selected for phase2");
    sqlx::query("UPDATE stage1_outputs SET usage_count = ?, last_usage = ? WHERE process_id = ?")
        .bind(8_i64)
        .bind(now - 2 * 86400)
        .bind(fresh_used.to_string())
        .execute(runtime.pool.as_ref())
        .await
        .expect("set fresh used metadata");

    let before_jobs_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE kind = 'memory_stage1'")
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("count stage1 jobs before prune");

    let pruned = runtime
        .prune_stage1_outputs_for_retention(30, 100)
        .await
        .expect("prune stage1 outputs");
    assert_eq!(pruned, 2);

    let remaining = sqlx::query_scalar::<_, String>(
        "SELECT process_id FROM stage1_outputs ORDER BY process_id",
    )
    .fetch_all(runtime.pool.as_ref())
    .await
    .expect("load remaining stage1 outputs");
    let mut expected_remaining = vec![fresh_used.to_string(), stale_selected.to_string()];
    expected_remaining.sort();
    assert_eq!(remaining, expected_remaining);

    let after_jobs_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE kind = 'memory_stage1'")
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("count stage1 jobs after prune");
    assert_eq!(after_jobs_count, before_jobs_count);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn prune_stage1_outputs_for_retention_respects_batch_limit() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
    let thread_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread a");
    let thread_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread b");
    let thread_c = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread c");

    for (process_id, workspace) in [
        (thread_a, "workspace-a"),
        (thread_b, "workspace-b"),
        (thread_c, "workspace-c"),
    ] {
        runtime
            .upsert_process(&test_process_metadata(
                &chaos_home,
                process_id,
                chaos_home.join(workspace),
            ))
            .await
            .expect("upsert thread");
    }

    let now = jiff::Timestamp::now().as_second();
    for (process_id, source_updated_at, summary) in [
        (thread_a, now - 60 * 86400, "stale-a"),
        (thread_b, now - 50 * 86400, "stale-b"),
        (thread_c, now - 40 * 86400, "stale-c"),
    ] {
        let claim = runtime
            .try_claim_stage1_job(process_id, owner, source_updated_at, 3600, 64)
            .await
            .expect("claim stage1");
        let ownership_token = match claim {
            Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
            other => panic!("unexpected stage1 claim outcome: {other:?}"),
        };
        assert!(
            runtime
                .mark_stage1_job_succeeded(
                    process_id,
                    ownership_token.as_str(),
                    source_updated_at,
                    &format!("raw-{summary}"),
                    summary,
                    None,
                )
                .await
                .expect("mark stage1 success"),
            "stage1 success should persist output"
        );
    }

    let pruned = runtime
        .prune_stage1_outputs_for_retention(30, 2)
        .await
        .expect("prune stage1 outputs with limit");
    assert_eq!(pruned, 2);

    let remaining_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stage1_outputs")
        .fetch_one(runtime.pool.as_ref())
        .await
        .expect("count remaining stage1 outputs");
    assert_eq!(remaining_count, 1);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn mark_stage1_job_succeeded_enqueues_global_consolidation() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    let thread_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id a");
    let thread_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("thread id b");
    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner id");

    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            thread_a,
            chaos_home.join("workspace-a"),
        ))
        .await
        .expect("upsert thread a");
    runtime
        .upsert_process(&test_process_metadata(
            &chaos_home,
            thread_b,
            chaos_home.join("workspace-b"),
        ))
        .await
        .expect("upsert thread b");

    let claim_a = runtime
        .try_claim_stage1_job(thread_a, owner, 100, 3600, 64)
        .await
        .expect("claim stage1 a");
    let token_a = match claim_a {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome for thread a: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(thread_a, token_a.as_str(), 100, "raw-a", "summary-a", None,)
            .await
            .expect("mark stage1 succeeded a"),
        "stage1 success should persist output for thread a"
    );

    let claim_b = runtime
        .try_claim_stage1_job(thread_b, owner, 101, 3600, 64)
        .await
        .expect("claim stage1 b");
    let token_b = match claim_b {
        Stage1JobClaimOutcome::Claimed { ownership_token } => ownership_token,
        other => panic!("unexpected stage1 claim outcome for thread b: {other:?}"),
    };
    assert!(
        runtime
            .mark_stage1_job_succeeded(thread_b, token_b.as_str(), 101, "raw-b", "summary-b", None,)
            .await
            .expect("mark stage1 succeeded b"),
        "stage1 success should persist output for thread b"
    );

    let claim = runtime
        .try_claim_global_phase2_job(owner, 3600)
        .await
        .expect("claim global consolidation");
    let input_watermark = match claim {
        Phase2JobClaimOutcome::Claimed {
            input_watermark, ..
        } => input_watermark,
        other => panic!("unexpected global consolidation claim outcome: {other:?}"),
    };
    assert_eq!(input_watermark, 101);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn phase2_global_lock_allows_only_one_fresh_runner() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    runtime
        .enqueue_global_consolidation(200)
        .await
        .expect("enqueue global consolidation");

    let owner_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner a");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner b");

    let running_claim = runtime
        .try_claim_global_phase2_job(owner_a, 3600)
        .await
        .expect("claim global lock");
    assert!(
        matches!(running_claim, Phase2JobClaimOutcome::Claimed { .. }),
        "first owner should claim global lock"
    );

    let second_claim = runtime
        .try_claim_global_phase2_job(owner_b, 3600)
        .await
        .expect("claim global lock from second owner");
    assert_eq!(second_claim, Phase2JobClaimOutcome::SkippedRunning);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn phase2_global_lock_stale_lease_allows_takeover() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    runtime
        .enqueue_global_consolidation(300)
        .await
        .expect("enqueue global consolidation");

    let owner_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner a");
    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner b");

    let initial_claim = runtime
        .try_claim_global_phase2_job(owner_a, 3600)
        .await
        .expect("claim initial global lock");
    let token_a = match initial_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token, ..
        } => ownership_token,
        other => panic!("unexpected initial claim outcome: {other:?}"),
    };

    sqlx::query("UPDATE jobs SET lease_until = ? WHERE kind = ? AND job_key = ?")
        .bind(jiff::Timestamp::now().as_second() - 1)
        .bind("memory_consolidate_global")
        .bind("global")
        .execute(runtime.pool.as_ref())
        .await
        .expect("expire global consolidation lease");

    let takeover_claim = runtime
        .try_claim_global_phase2_job(owner_b, 3600)
        .await
        .expect("claim stale global lock");
    let (token_b, input_watermark) = match takeover_claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => (ownership_token, input_watermark),
        other => panic!("unexpected takeover claim outcome: {other:?}"),
    };
    assert_ne!(token_a, token_b);
    assert_eq!(input_watermark, 300);

    assert_eq!(
        runtime
            .mark_global_phase2_job_succeeded(token_a.as_str(), 300, &[])
            .await
            .expect("mark stale owner success result"),
        false,
        "stale owner should lose finalization ownership after takeover"
    );
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(token_b.as_str(), 300, &[])
            .await
            .expect("mark takeover owner success"),
        "takeover owner should finalize consolidation"
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn phase2_backfilled_inputs_below_last_success_still_become_dirty() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    runtime
        .enqueue_global_consolidation(500)
        .await
        .expect("enqueue initial consolidation");
    let owner_a = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner a");
    let claim_a = runtime
        .try_claim_global_phase2_job(owner_a, 3_600)
        .await
        .expect("claim initial consolidation");
    let token_a = match claim_a {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => {
            assert_eq!(input_watermark, 500);
            ownership_token
        }
        other => panic!("unexpected initial phase2 claim outcome: {other:?}"),
    };
    assert!(
        runtime
            .mark_global_phase2_job_succeeded(token_a.as_str(), 500, &[])
            .await
            .expect("mark initial phase2 success"),
        "initial phase2 success should finalize"
    );

    runtime
        .enqueue_global_consolidation(400)
        .await
        .expect("enqueue backfilled consolidation");

    let owner_b = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner b");
    let claim_b = runtime
        .try_claim_global_phase2_job(owner_b, 3_600)
        .await
        .expect("claim backfilled consolidation");
    match claim_b {
        Phase2JobClaimOutcome::Claimed {
            input_watermark, ..
        } => {
            assert!(
                input_watermark > 500,
                "backfilled enqueue should advance dirty watermark beyond last success"
            );
        }
        other => panic!("unexpected backfilled phase2 claim outcome: {other:?}"),
    }

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn phase2_failure_fallback_updates_unowned_running_job() {
    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");

    runtime
        .enqueue_global_consolidation(400)
        .await
        .expect("enqueue global consolidation");

    let owner = ProcessId::from_string(&Uuid::new_v4().to_string()).expect("owner");
    let claim = runtime
        .try_claim_global_phase2_job(owner, 3_600)
        .await
        .expect("claim global consolidation");
    let ownership_token = match claim {
        Phase2JobClaimOutcome::Claimed {
            ownership_token, ..
        } => ownership_token,
        other => panic!("unexpected claim outcome: {other:?}"),
    };

    sqlx::query("UPDATE jobs SET ownership_token = NULL WHERE kind = ? AND job_key = ?")
        .bind("memory_consolidate_global")
        .bind("global")
        .execute(runtime.pool.as_ref())
        .await
        .expect("clear ownership token");

    assert_eq!(
        runtime
            .mark_global_phase2_job_failed(ownership_token.as_str(), "lost", 3_600)
            .await
            .expect("mark phase2 failed with strict ownership"),
        false,
        "strict failure update should not match unowned running job"
    );
    assert!(
        runtime
            .mark_global_phase2_job_failed_if_unowned(ownership_token.as_str(), "lost", 3_600)
            .await
            .expect("fallback failure update should match unowned running job"),
        "fallback failure update should transition the unowned running job"
    );

    let claim = runtime
        .try_claim_global_phase2_job(ProcessId::new(), 3_600)
        .await
        .expect("claim after fallback failure");
    assert_eq!(claim, Phase2JobClaimOutcome::SkippedNotDirty);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}
