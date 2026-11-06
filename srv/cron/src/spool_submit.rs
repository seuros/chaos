//! Queued → InProgress submit path, driven as a saga.
//!
//! Callers hand over a batch of `(custom_id, TurnRequest)` items plus the
//! backend they want to target. The five forward steps are: persist the
//! queued row, dispatch to the backend, persist the returned `batch_id`,
//! create the poll cron row, and retire replaced poll rows. A failure after
//! the provider accepted the batch compensates in reverse order, which is the
//! only path that invokes `SpoolBackend::cancel`. Progress is persisted next
//! to the manifest in `spool_jobs.saga_json`, so a restart resumes or
//! compensates instead of leaving a billing batch nobody polls.

use std::sync::Arc;

use chaos_abi::SpoolRegistry;
use chaos_abi::TurnRequest;
use chaos_snitch::tracing::info;
use chaos_snitch::tracing::warn;
use chaos_vfs::ChaosVfs;
use serde::Deserialize;
use serde::Serialize;

use crate::job::CronJob;
use crate::provider::BackendCronStorage;
use crate::spool_store::BackendSpoolStore;

mod effects;
mod saga;

use effects::StoreEffects;
use saga::SagaState;
use saga::SubmitRun;
use saga::drive;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PollWiring {
    pub name: String,
    pub schedule_json: String,
    pub project_path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SubmitReceipt {
    pub batch_id: String,
    pub poll_job: Option<CronJob>,
    pub replaced_poll_rows: u64,
}

/// Side effects of each saga step, so the driver stays free of storage and
/// provider details and tests can fail any step deterministically.
pub(crate) trait SubmitEffects {
    async fn insert_queued(&mut self) -> Result<(), String>;
    async fn submit(&mut self) -> Result<String, String>;
    async fn insert_submitted(&mut self, batch_id: &str) -> Result<(), String>;
    async fn create_poll_row(&mut self, poll: &PollWiring) -> Result<CronJob, String>;
    async fn retire_poll_rows(&mut self, keep: &str) -> Result<u64, String>;
    async fn cancel(&mut self, batch_id: &str) -> Result<(), String>;
    async fn mark_failed(&mut self, error: &str) -> Result<(), String>;
    async fn delete_poll_row(&mut self, job_id: &str) -> Result<(), String>;
    async fn persist(
        &mut self,
        state: Option<&SagaState>,
        batch_id: Option<&str>,
    ) -> Result<(), String>;
}

/// Submit a batch of turns to `backend_name`, persist the `spool_jobs` row in
/// `InProgress`, and wire the poll cron row when `poll` is given.
pub(crate) async fn submit_manifest(
    registry: &SpoolRegistry,
    store: &BackendSpoolStore,
    cron: &BackendCronStorage,
    manifest_id: &str,
    backend_name: &str,
    items: Vec<(String, TurnRequest)>,
    poll: Option<PollWiring>,
) -> Result<SubmitReceipt, String> {
    let mut effects = StoreEffects::new(registry, store, cron, manifest_id, backend_name, items)?;
    drive(&mut effects, SubmitRun::fresh(manifest_id, poll)).await
}

/// Convenience wrapper: build the stores from a provider and submit.
///
/// Returns the backend-assigned `batch_id`.
pub async fn submit_manifest_from_provider(
    registry: &SpoolRegistry,
    provider: &ChaosVfs,
    manifest_id: &str,
    backend_name: &str,
    items: Vec<(String, TurnRequest)>,
) -> Result<String, String> {
    let store = BackendSpoolStore::from_provider(provider);
    let cron = BackendCronStorage::from_provider(provider);
    submit_manifest(
        registry,
        &store,
        &cron,
        manifest_id,
        backend_name,
        items,
        None,
    )
    .await
    .map(|receipt| receipt.batch_id)
}

/// Pick up every submit saga a previous process left unfinished and drive it
/// to completion or compensation. A saga interrupted before the backend
/// handed back a batch id cannot be replayed (the turn payloads are not
/// persisted) and is compensated.
pub(crate) async fn resume_pending(
    registry: &SpoolRegistry,
    store: &BackendSpoolStore,
    cron: &BackendCronStorage,
) -> Vec<(String, Result<SubmitReceipt, String>)> {
    let rows = match store.list_pending_sagas().await {
        Ok(rows) => rows,
        Err(e) => return vec![(String::new(), Err(format!("list pending sagas: {e}")))],
    };
    let mut outcomes = Vec::with_capacity(rows.len());
    for row in &rows {
        let outcome = match (
            SubmitRun::restored(row),
            StoreEffects::resumed(registry, store, cron, row),
        ) {
            (Ok(run), Ok(mut effects)) => drive(&mut effects, run).await,
            (Err(e), _) | (_, Err(e)) => Err(e),
        };
        outcomes.push((row.manifest_id.clone(), outcome));
    }
    outcomes
}

/// Resume unfinished submit sagas against the stores behind `provider`.
pub async fn resume_spool_submits(registry: Arc<SpoolRegistry>, provider: &ChaosVfs) {
    let store = BackendSpoolStore::from_provider(provider);
    let cron = BackendCronStorage::from_provider(provider);
    for (manifest_id, outcome) in resume_pending(&registry, &store, &cron).await {
        match outcome {
            Ok(receipt) => info!(
                manifest_id,
                batch_id = receipt.batch_id,
                "resumed spool submit"
            ),
            Err(error) => warn!(manifest_id, error, "spool submit saga"),
        }
    }
}

#[cfg(test)]
mod tests;
