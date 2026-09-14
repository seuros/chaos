use super::*;

const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

fn daily_schedule_json() -> String {
    Schedule::Interval { seconds: 86_400 }.to_json()
}

fn postgres_test_url() -> Option<String> {
    std::env::var(TEST_DATABASE_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn open_postgres_store() -> Option<(PgPool, PostgresCronStore)> {
    let database_url = postgres_test_url()?;
    let pool = chaos_proc::open_runtime_db_postgres_url(&database_url)
        .await
        .expect("open postgres runtime db");
    Some((pool.clone(), PostgresCronStore::new(pool)))
}

/// A project path only this test owns. The table is shared with whatever
/// else runs against the same database, so a test reads back its own rows
/// rather than the whole table.
fn test_project_path(test: &str) -> String {
    format!("/tmp/chaos-postgres/{test}/{}", std::process::id())
}

fn test_params(name: &str) -> CreateJobParams {
    CreateJobParams::shell(
        name.to_string(),
        daily_schedule_json(),
        "echo hi".to_string(),
        CronScope::Project,
        None,
        None,
    )
}

#[tokio::test]
async fn create_uses_short_hex_ids() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let pool = chaos_proc::open_runtime_db(temp_dir.path())
        .await
        .expect("open runtime db");
    let store = CronStore::new(pool);

    let job = store
        .create(&test_params("short-id"))
        .await
        .expect("create cron job");

    assert_eq!(job.id.len(), JOB_ID_HEX_CHARS);
    assert!(
        job.id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        "id should be lowercase hex"
    );
}

#[tokio::test]
async fn create_retries_on_id_collision() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let pool = chaos_proc::open_runtime_db(temp_dir.path())
        .await
        .expect("open runtime db");
    let store = CronStore::new(pool);

    let first = store
        .create_with_id_generator(&test_params("first"), || "deadbeef".to_string())
        .await
        .expect("create initial cron job");
    assert_eq!(first.id, "deadbeef");

    let mut ids = ["deadbeef", "cafebabe"].into_iter();
    let second = store
        .create_with_id_generator(&test_params("second"), || {
            ids.next().expect("have another id").to_string()
        })
        .await
        .expect("retry after collision");

    assert_eq!(second.id, "cafebabe");
}

#[tokio::test]
async fn session_scoped_jobs_round_trip_session_id() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let pool = chaos_proc::open_runtime_db(temp_dir.path())
        .await
        .expect("open runtime db");
    let store = CronStore::new(pool.clone());

    let mut params = CreateJobParams::shell(
        "session-job".to_string(),
        daily_schedule_json(),
        "echo hi".to_string(),
        CronScope::Session,
        None,
        Some("session-123".to_string()),
    );
    params.execution_policy = Some(r#"{"version":1}"#.to_string());
    let job = store
        .create(&params)
        .await
        .expect("create session cron job");

    let listed = store.list(None, None).await.expect("list cron jobs");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, job.id);
    assert_eq!(listed[0].session_id.as_deref(), Some("session-123"));
    assert_eq!(listed[0].execution_policy, params.execution_policy);
    assert!(
        serde_json::to_value(&listed[0])
            .unwrap()
            .get("execution_policy")
            .is_none()
    );

    let fetched = store
        .get(&job.id)
        .await
        .expect("get cron job")
        .expect("job exists");
    assert_eq!(fetched.session_id.as_deref(), Some("session-123"));
    assert_eq!(fetched.execution_policy, params.execution_policy);

    sqlx::query("UPDATE cron_jobs SET next_run_at = 0 WHERE id = ?")
        .bind(&job.id)
        .execute(&pool)
        .await
        .expect("force job due");

    let due = store.due_jobs(1).await.expect("list due jobs");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].session_id.as_deref(), Some("session-123"));
    assert_eq!(due[0].execution_policy, params.execution_policy);
    assert_eq!(
        store.due_now().await.unwrap()[0].execution_policy,
        params.execution_policy
    );
}

#[tokio::test]
async fn enabled_jobs_can_clear_next_run_at_after_running() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let pool = chaos_proc::open_runtime_db(temp_dir.path())
        .await
        .expect("open runtime db");
    let store = CronStore::new(pool);

    let job = store
        .create(&test_params("clear-next-run"))
        .await
        .expect("create cron job");

    store
        .mark_run(&job.id, None)
        .await
        .expect("mark job run without another occurrence");

    let refreshed = store
        .get(&job.id)
        .await
        .expect("get cron job")
        .expect("job exists");
    assert!(refreshed.enabled, "job should remain enabled");
    assert!(
        refreshed.last_run_at.is_some(),
        "last_run_at should be updated"
    );
    assert_eq!(refreshed.next_run_at, None);
}

#[tokio::test]
async fn enabling_an_already_enabled_job_preserves_next_run_at() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let pool = chaos_proc::open_runtime_db(temp_dir.path())
        .await
        .expect("open runtime db");
    let store = CronStore::new(pool.clone());

    let job = store
        .create(&test_params("idempotent-enable"))
        .await
        .expect("create cron job");

    let expected_next_run_at = 4_242_424_242_i64;
    sqlx::query("UPDATE cron_jobs SET next_run_at = ? WHERE id = ?")
        .bind(expected_next_run_at)
        .bind(&job.id)
        .execute(&pool)
        .await
        .expect("set sentinel next_run_at");

    store
        .set_enabled(&job.id, true)
        .await
        .expect("re-enable already enabled job");

    let refreshed = store
        .get(&job.id)
        .await
        .expect("get cron job")
        .expect("job exists");
    assert!(refreshed.enabled, "job should remain enabled");
    assert_eq!(refreshed.next_run_at, Some(expected_next_run_at));
}

#[tokio::test]
async fn postgres_create_retries_on_id_collision() {
    let Some((_pool, store)) = open_postgres_store().await else {
        eprintln!("skipping postgres cron store validation; {TEST_DATABASE_URL_ENV} is not set");
        return;
    };

    let taken = format!("{:08x}", std::process::id());
    let free = format!("{:08x}", !std::process::id());

    let first = store
        .create_with_id_generator(&test_params("postgres-first"), || taken.clone())
        .await
        .expect("create initial postgres cron job");
    assert_eq!(first.id, taken);

    let mut ids = [taken.clone(), free.clone()].into_iter();
    let second = store
        .create_with_id_generator(&test_params("postgres-second"), || {
            ids.next().expect("have another id")
        })
        .await
        .expect("retry after postgres collision");

    assert_eq!(second.id, free);

    store.delete(&taken).await.expect("delete first job");
    store.delete(&free).await.expect("delete second job");
}

#[tokio::test]
async fn postgres_store_round_trips_session_jobs_and_due_views() {
    let Some((pool, store)) = open_postgres_store().await else {
        eprintln!("skipping postgres cron store validation; {TEST_DATABASE_URL_ENV} is not set");
        return;
    };

    let project_path = test_project_path("round-trip");
    let session_id = format!("postgres-session-{}", std::process::id());

    let mut params = CreateJobParams::shell(
        "postgres-session-job".to_string(),
        daily_schedule_json(),
        "echo hi".to_string(),
        CronScope::Session,
        Some(project_path.clone()),
        Some(session_id.clone()),
    );
    params.execution_policy = Some(r#"{"version":1}"#.to_string());
    let job = store
        .create(&params)
        .await
        .expect("create postgres session cron job");

    let listed = store
        .list(None, Some(&project_path))
        .await
        .expect("list cron jobs");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, job.id);
    assert_eq!(listed[0].session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(listed[0].execution_policy, params.execution_policy);

    let fetched = store
        .get(&job.id)
        .await
        .expect("get cron job")
        .expect("job exists");
    assert_eq!(fetched.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(fetched.project_path.as_deref(), Some(project_path.as_str()));
    assert_eq!(fetched.execution_policy, params.execution_policy);

    sqlx::query("UPDATE cron_jobs SET next_run_at = $1 WHERE id = $2")
        .bind(0_i64)
        .bind(&job.id)
        .execute(&pool)
        .await
        .expect("force postgres job due");

    let due = store.due_jobs(64).await.expect("list due jobs");
    let due_job = due
        .iter()
        .find(|candidate| candidate.id == job.id)
        .expect("the forced-due job should be due");
    assert_eq!(due_job.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(due_job.execution_policy, params.execution_policy);

    let due_now = store.due_now().await.expect("list due-now jobs");
    assert!(
        due_now.iter().any(|candidate| candidate.id == job.id),
        "due_cron_jobs view should include the forced-due job"
    );
    assert_eq!(
        due_now
            .iter()
            .find(|candidate| candidate.id == job.id)
            .unwrap()
            .execution_policy,
        params.execution_policy
    );

    store
        .mark_run(&job.id, None)
        .await
        .expect("mark postgres cron job run");

    let refreshed = store
        .get(&job.id)
        .await
        .expect("get refreshed cron job")
        .expect("job exists");
    assert!(refreshed.enabled, "job should remain enabled");
    assert!(
        refreshed.last_run_at.is_some(),
        "last_run_at should be updated"
    );
    assert_eq!(refreshed.next_run_at, None);

    store
        .delete(&job.id)
        .await
        .expect("delete postgres cron job");
    assert!(
        store
            .list(None, Some(&project_path))
            .await
            .expect("list cron jobs after delete")
            .is_empty(),
        "the job should be gone after delete"
    );
}
