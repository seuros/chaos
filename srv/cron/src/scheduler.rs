//! Tick-based scheduler that polls for due jobs and executes them.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::job::CronJob;
use crate::job::JobKind;
use crate::provider::BackendCronStorage;
use crate::schedule::Schedule;
use chaos_vfs::ChaosVfs;
use tokio::sync::watch;
use tracing::error;
use tracing::info;
use tracing::warn;

/// Default tick interval for the scheduler (30 seconds).
const DEFAULT_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Callback that receives a due job and executes it. Returns Ok(output) on
/// success or Err(message) on failure. The scheduler logs the outcome either
/// way and always advances next_run_at afterward.
pub type JobExecutor = Arc<
    dyn Fn(&CronJob) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync,
>;

/// Compose a shell executor and a spool executor into one dispatcher keyed
/// on `job.kind`. Unknown kinds error out.
pub fn dispatch_executor(shell: JobExecutor, spool: JobExecutor) -> JobExecutor {
    Arc::new(move |job| {
        let shell = shell.clone();
        let spool = spool.clone();
        match job.kind.as_str() {
            JobKind::SHELL_TAG => shell(job),
            JobKind::SPOOL_TAG => spool(job),
            other => {
                let msg = format!("unknown cron job kind: {other}");
                Box::pin(async move { Err(msg) })
            }
        }
    })
}

/// Process-wide scheduler guard. Ensures only one scheduler runs per process,
/// even when multiple sessions are created.
static SCHEDULER_GUARD: OnceLock<watch::Sender<bool>> = OnceLock::new();

/// Spawn the global cron scheduler if it hasn't been started yet.
///
/// Uses `OnceLock` to guarantee at most one scheduler instance per process.
/// Returns the shutdown sender on first call and `None` on subsequent or
/// concurrent calls that lose the initialization race.
/// The scheduler runs in a background `tokio::spawn` task until the shutdown
/// sender is dropped or `true` is sent.
pub fn spawn_global(
    provider: &ChaosVfs,
    executor: JobExecutor,
) -> Option<&'static watch::Sender<bool>> {
    if SCHEDULER_GUARD.get().is_some() {
        return None;
    }

    let store = BackendCronStorage::from_provider(provider);
    let (shutdown_tx, shutdown_rx) = Scheduler::shutdown_channel();
    if SCHEDULER_GUARD.set(shutdown_tx).is_err() {
        return None;
    }

    let scheduler = Scheduler::new(store, executor, DEFAULT_TICK_INTERVAL, shutdown_rx);
    tokio::spawn(scheduler.run());

    SCHEDULER_GUARD.get()
}

/// The scheduler runs a background tick loop, checking for due jobs
/// and dispatching them for execution.
pub struct Scheduler {
    store: BackendCronStorage,
    executor: JobExecutor,
    tick_interval: std::time::Duration,
    shutdown_rx: watch::Receiver<bool>,
}

impl Scheduler {
    pub(crate) fn new(
        store: BackendCronStorage,
        executor: JobExecutor,
        tick_interval: std::time::Duration,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            store,
            executor,
            tick_interval,
            shutdown_rx,
        }
    }

    /// Run the scheduler loop until shutdown is signalled.
    pub async fn run(mut self) {
        info!(
            "cron scheduler started, tick interval: {:?}",
            self.tick_interval
        );
        let mut interval = tokio::time::interval(self.tick_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.tick().await;
                }
                changed = self.shutdown_rx.changed() => {
                    if changed.is_err() || *self.shutdown_rx.borrow() {
                        info!("cron scheduler shutting down");
                        break;
                    }
                }
            }
        }
    }

    async fn tick(&self) {
        let jobs = match self.store.due_now().await {
            Ok(jobs) => jobs,
            Err(err) => {
                warn!("cron tick: failed to fetch due jobs: {err}");
                return;
            }
        };

        for job in &jobs {
            if *self.shutdown_rx.borrow() || self.shutdown_rx.has_changed().is_err() {
                break;
            }
            info!(job_id = %job.id, name = %job.name, "executing cron job");

            // Execute the command before advancing next_run_at.
            match (self.executor)(job).await {
                Ok(output) => {
                    if !output.is_empty() {
                        info!(job_id = %job.id, "cron job output: {output}");
                    }
                }
                Err(msg) => {
                    error!(job_id = %job.id, "cron job failed: {msg}");
                }
            }

            let next_run_at = Schedule::parse(&job.schedule)
                .and_then(|s| s.next_after(jiff::Timestamp::now()))
                .ok();

            if let Err(err) = self.store.mark_run(&job.id, next_run_at).await {
                warn!(job_id = %job.id, "failed to mark job run: {err}");
            }
        }
    }

    /// Create a shutdown channel pair. Send `true` to stop the scheduler.
    pub fn shutdown_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }
}
