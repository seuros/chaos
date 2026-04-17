//! Tick-based scheduler that polls for due jobs and executes them.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::job::CronJob;
use crate::job::CronScope;
use crate::job::JobKind;
use crate::provider::BackendCronStorage;
use crate::schedule::Schedule;
use chaos_storage::ChaosStorageProvider;
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

/// Default executor — runs job.command as a shell command via `sh -c`.
pub fn shell_executor() -> JobExecutor {
    Arc::new(|job: &CronJob| {
        let command = job.command.clone();
        let job_id = job.id.clone();
        let job_scope = job.scope;
        let job_project_path = job.project_path.clone();
        Box::pin(async move {
            let mut spawned_command = tokio::process::Command::new("sh");
            spawned_command.arg("-c").arg(&command);
            if matches!(job_scope, CronScope::Project) {
                let project_path = job_project_path.as_deref().ok_or_else(|| {
                    format!("project-scoped job {job_id} is missing project_path metadata")
                })?;
                spawned_command.current_dir(project_path);
            }

            let output = spawned_command
                .output()
                .await
                .map_err(|e| format!("failed to spawn command for job {job_id}: {e}"))?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                Ok(format!("{stdout}{stderr}").trim().to_string())
            } else {
                Err(format!(
                    "job {job_id} exited with {}: {stderr}",
                    output.status
                ))
            }
        })
    })
}

/// Process-wide scheduler guard. Ensures only one scheduler runs per process,
/// even when multiple sessions are created.
static SCHEDULER_GUARD: OnceLock<watch::Sender<bool>> = OnceLock::new();

/// Spawn the global cron scheduler if it hasn't been started yet.
///
/// Uses `OnceLock` to guarantee at most one scheduler instance per process.
/// Returns the shutdown sender on first call, `Ok(None)` on subsequent calls,
/// or an error when the storage provider cannot supply a supported backend.
/// The scheduler runs in a background `tokio::spawn` task until the shutdown
/// sender is dropped or `true` is sent.
pub fn spawn_global(
    provider: &ChaosStorageProvider,
    executor: JobExecutor,
) -> Result<Option<&'static watch::Sender<bool>>, String> {
    if SCHEDULER_GUARD.get().is_some() {
        return Ok(None);
    }

    let store = BackendCronStorage::from_provider(provider)?;
    let (shutdown_tx, shutdown_rx) = Scheduler::shutdown_channel();
    if SCHEDULER_GUARD.set(shutdown_tx).is_err() {
        return Ok(None);
    }

    let scheduler = Scheduler::new(store, executor, DEFAULT_TICK_INTERVAL, shutdown_rx);
    tokio::spawn(scheduler.run());

    Ok(SCHEDULER_GUARD.get())
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
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!("cron scheduler shutting down");
                        break;
                    }
                }
            }
        }
    }

    async fn tick(&self) {
        let now_ts = jiff::Timestamp::now();
        let jobs = match self.store.due_now().await {
            Ok(jobs) => jobs,
            Err(err) => {
                warn!("cron tick: failed to fetch due jobs: {err}");
                return;
            }
        };

        for job in &jobs {
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
                .and_then(|s| s.next_after(now_ts))
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
