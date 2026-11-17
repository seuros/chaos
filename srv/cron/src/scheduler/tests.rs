use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use chaos_journald::JournalClient;
use chaos_vfs::MountConfig;
use tokio::sync::Notify;

use super::*;
use crate::CronStorage;
use crate::job::CreateJobParams;
use crate::job::CronScope;

async fn mount(dir: &std::path::Path) -> ChaosVfs {
    ChaosVfs::from_config(MountConfig::sqlite_home(dir))
        .await
        .expect("mount sqlite home")
}

fn scheduler(
    provider: &ChaosVfs,
    executor: JobExecutor,
    ttl: Duration,
) -> (Scheduler, watch::Sender<bool>) {
    let pool = provider.sqlite_pool().expect("sqlite pool");
    let journal: JournalConnector = Arc::new(move || {
        let pool = pool.clone();
        Box::pin(async move { Ok(JournalClient::sqlite_pool(pool)) })
    });
    let (shutdown_tx, shutdown_rx) = Scheduler::shutdown_channel();
    let scheduler = Scheduler::new(
        BackendCronStorage::from_provider(provider),
        executor,
        SchedulerLease::new(journal, ttl),
        DEFAULT_TICK_INTERVAL,
        shutdown_rx,
    );
    (scheduler, shutdown_tx)
}

fn counting(runs: Arc<AtomicUsize>, gate: Option<Arc<Notify>>) -> JobExecutor {
    Arc::new(move |_job| {
        let runs = runs.clone();
        let gate = gate.clone();
        Box::pin(async move {
            if runs.fetch_add(1, SeqCst) == 0
                && let Some(gate) = gate
            {
                gate.notified().await;
            }
            Ok(String::new())
        })
    })
}

async fn due_job(provider: &ChaosVfs) -> CronJob {
    let store = BackendCronStorage::from_provider(provider);
    let job = store
        .create(&CreateJobParams::shell(
            "lease".to_string(),
            Schedule::Interval { seconds: 3_600 }.to_json(),
            "true".to_string(),
            CronScope::Project,
            None,
            None,
        ))
        .await
        .expect("create job");
    force_due(provider, &job.id).await;
    job
}

async fn force_due(provider: &ChaosVfs, id: &str) {
    sqlx::query("UPDATE cron_jobs SET next_run_at = 0 WHERE id = ?")
        .bind(id)
        .execute(&provider.sqlite_pool().expect("sqlite pool"))
        .await
        .expect("force job due");
}

#[tokio::test]
async fn two_schedulers_on_one_store_run_a_due_job_once() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = mount(tmp.path()).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let executor = counting(runs.clone(), Some(gate.clone()));
    let (mut first, _first_tx) = scheduler(&provider, executor.clone(), DEFAULT_LEASE_TTL);
    let (mut second, _second_tx) = scheduler(&provider, executor, DEFAULT_LEASE_TTL);
    let job = due_job(&provider).await;

    let overlapping = async {
        while runs.load(SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        second.tick().await;
        gate.notify_one();
    };
    tokio::join!(first.tick(), overlapping);

    assert_eq!(runs.load(SeqCst), 1);
    let advanced = BackendCronStorage::from_provider(&provider)
        .get(&job.id)
        .await
        .expect("get job")
        .expect("job exists");
    assert!(advanced.next_run_at.is_some_and(|at| at > 0));
}

#[tokio::test]
async fn dead_holder_hands_over_after_lease_expiry() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = mount(tmp.path()).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let ttl = Duration::from_secs(1);
    let job = due_job(&provider).await;

    let (mut holder, _holder_tx) = scheduler(&provider, counting(runs.clone(), None), ttl);
    holder.tick().await;
    assert_eq!(runs.load(SeqCst), 1);
    drop(holder);

    force_due(&provider, &job.id).await;
    let (mut successor, _successor_tx) = scheduler(&provider, counting(runs.clone(), None), ttl);
    successor.tick().await;
    assert_eq!(
        runs.load(SeqCst),
        1,
        "a live lease keeps the successor idle"
    );

    tokio::time::sleep(ttl + Duration::from_millis(200)).await;
    successor.tick().await;
    assert_eq!(
        runs.load(SeqCst),
        2,
        "the successor takes over once the lease expires"
    );
}
