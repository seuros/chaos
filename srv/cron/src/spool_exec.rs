//! JobExecutor that drives a `kind=spool` cron row one tick forward.

use std::sync::Arc;

use chaos_abi::SpoolPhase;
use chaos_abi::SpoolRegistry;
use chaos_vfs::ChaosVfs;

use crate::provider::BackendCronStorage;
use crate::provider::CronStorage;
use crate::scheduler::JobExecutor;
use crate::spool_store::BackendSpoolStore;

/// Build an executor that polls a single spool job on each tick.
///
/// On terminal phases (`Completed` / `Failed` / `Expired` / `Cancelled`) the
/// executor fetches results (Completed only), persists them into `spool_jobs`,
/// disables the driving cron row, and returns `Ok(status)`.
pub(crate) fn spool_executor(
    registry: Arc<SpoolRegistry>,
    store: BackendSpoolStore,
    cron_store: BackendCronStorage,
) -> JobExecutor {
    Arc::new(move |job| {
        let registry = registry.clone();
        let store = store.clone();
        let cron_store = cron_store.clone();
        let manifest_id = job.manifest_id.clone();
        let job_id = job.id.clone();
        Box::pin(async move {
            let manifest_id = manifest_id
                .ok_or_else(|| format!("spool-kind job {job_id} is missing manifest_id"))?;
            let row = store
                .load(&manifest_id)
                .await
                .map_err(|e| format!("load spool row {manifest_id}: {e}"))?
                .ok_or_else(|| format!("spool row {manifest_id} not found"))?;

            if is_terminal(&row.status) {
                disable_cron_job(&cron_store, &job_id).await?;
                return Ok(format!("spool {manifest_id} already {}", row.status));
            }

            let batch_id = row
                .batch_id
                .as_deref()
                .ok_or_else(|| format!("spool row {manifest_id} has no batch_id yet"))?;

            let backend = registry
                .get(&row.backend)
                .ok_or_else(|| format!("no spool backend registered for {}", row.backend))?;

            let report = backend
                .poll(batch_id)
                .await
                .map_err(|e| format!("poll {manifest_id}: {e}"))?;

            match report.phase {
                SpoolPhase::InProgress => Ok(format!(
                    "spool {manifest_id} in_progress: {}",
                    report.raw_provider_status
                )),
                SpoolPhase::Completed => {
                    let results = backend
                        .fetch_results(batch_id)
                        .await
                        .map_err(|e| format!("fetch {manifest_id}: {e}"))?;
                    let result_json = serde_json::to_string(&results)
                        .map_err(|e| format!("serialize results for {manifest_id}: {e}"))?;
                    store
                        .mark_terminal(
                            &manifest_id,
                            SpoolPhase::Completed,
                            Some(&result_json),
                            None,
                        )
                        .await
                        .map_err(|e| format!("persist completion {manifest_id}: {e}"))?;
                    disable_cron_job(&cron_store, &job_id).await?;
                    Ok(format!(
                        "spool {manifest_id} completed with {} results",
                        results.len()
                    ))
                }
                terminal @ (SpoolPhase::Failed | SpoolPhase::Expired | SpoolPhase::Cancelled) => {
                    store
                        .mark_terminal(
                            &manifest_id,
                            terminal,
                            None,
                            Some(&report.raw_provider_status),
                        )
                        .await
                        .map_err(|e| format!("persist terminal {manifest_id}: {e}"))?;
                    disable_cron_job(&cron_store, &job_id).await?;
                    Ok(format!("spool {manifest_id} {terminal:?}"))
                }
            }
        })
    })
}

/// Constructing the backend adapters is infallible; storage and provider
/// failures are reported when the returned executor runs a job.
pub fn spool_executor_from_provider(
    registry: Arc<SpoolRegistry>,
    provider: &ChaosVfs,
) -> JobExecutor {
    let spool_store = BackendSpoolStore::from_provider(provider);
    let cron_store = BackendCronStorage::from_provider(provider);
    spool_executor(registry, spool_store, cron_store)
}

async fn disable_cron_job(cron_store: &BackendCronStorage, job_id: &str) -> Result<(), String> {
    cron_store
        .set_enabled(job_id, false)
        .await
        .map_err(|e| format!("disable cron job {job_id}: {e}"))
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "Completed" | "Failed" | "Expired" | "Cancelled")
}

#[cfg(test)]
mod tests;
