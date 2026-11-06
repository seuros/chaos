//! `SubmitEffects` backed by the spool and cron stores plus the backend
//! registry.

use std::sync::Arc;

use chaos_abi::SpoolRegistry;
use chaos_abi::TurnRequest;

use super::PollWiring;
use super::SubmitEffects;
use super::saga::SagaState;
use crate::CronStorage;
use crate::job::CreateJobParams;
use crate::job::CronJob;
use crate::job::CronScope;
use crate::provider::BackendCronStorage;
use crate::spool_store::BackendSpoolStore;
use crate::spool_store::SpoolRow;

pub(super) struct StoreEffects<'a> {
    registry: &'a SpoolRegistry,
    store: &'a BackendSpoolStore,
    cron: &'a BackendCronStorage,
    manifest_id: &'a str,
    backend_name: &'a str,
    request_count: u32,
    payload_json: String,
    items: Option<Vec<(String, TurnRequest)>>,
}

impl<'a> StoreEffects<'a> {
    pub(super) fn new(
        registry: &'a SpoolRegistry,
        store: &'a BackendSpoolStore,
        cron: &'a BackendCronStorage,
        manifest_id: &'a str,
        backend_name: &'a str,
        items: Vec<(String, TurnRequest)>,
    ) -> Result<Self, String> {
        if items.is_empty() {
            return Err(format!("manifest {manifest_id} has no items to submit"));
        }
        let request_count = u32::try_from(items.len())
            .map_err(|_| format!("too many items for manifest {manifest_id}"))?;
        let custom_ids: Vec<&str> = items.iter().map(|(id, _)| id.as_str()).collect();
        let payload_json = serde_json::to_string(&custom_ids)
            .map_err(|e| format!("serialize custom ids for {manifest_id}: {e}"))?;
        Ok(Self {
            registry,
            store,
            cron,
            manifest_id,
            backend_name,
            request_count,
            payload_json,
            items: Some(items),
        })
    }

    pub(super) fn resumed(
        registry: &'a SpoolRegistry,
        store: &'a BackendSpoolStore,
        cron: &'a BackendCronStorage,
        row: &'a SpoolRow,
    ) -> Result<Self, String> {
        let request_count = u32::try_from(row.request_count)
            .map_err(|_| format!("invalid request count for manifest {}", row.manifest_id))?;
        Ok(Self {
            registry,
            store,
            cron,
            manifest_id: &row.manifest_id,
            backend_name: &row.backend,
            request_count,
            payload_json: row.payload_json.clone(),
            items: None,
        })
    }

    fn backend(&self) -> Result<Arc<dyn chaos_abi::SpoolBackend>, String> {
        self.registry
            .get(self.backend_name)
            .ok_or_else(|| format!("no spool backend registered for {}", self.backend_name))
    }
}

impl SubmitEffects for StoreEffects<'_> {
    async fn insert_queued(&mut self) -> Result<(), String> {
        self.store
            .insert_queued(
                self.manifest_id,
                self.backend_name,
                self.request_count,
                &self.payload_json,
            )
            .await
            .map_err(|e| format!("persist queued submit {}: {e}", self.manifest_id))
    }

    async fn submit(&mut self) -> Result<String, String> {
        let backend = self.backend()?;
        let items = self.items.take().ok_or_else(|| {
            format!(
                "submit {} was interrupted before the batch id was recorded",
                self.manifest_id
            )
        })?;
        backend
            .submit(items)
            .await
            .map_err(|e| format!("submit {}: {e}", self.manifest_id))
    }

    async fn insert_submitted(&mut self, batch_id: &str) -> Result<(), String> {
        self.store
            .insert_submitted(
                self.manifest_id,
                self.backend_name,
                batch_id,
                self.request_count,
                &self.payload_json,
            )
            .await
            .map_err(|e| format!("persist submit {}: {e}", self.manifest_id))
    }

    async fn create_poll_row(&mut self, poll: &PollWiring) -> Result<CronJob, String> {
        let params = CreateJobParams::spool(
            poll.name.clone(),
            poll.schedule_json.clone(),
            self.manifest_id.to_string(),
            CronScope::Project,
            Some(poll.project_path.clone()),
            None,
        );
        self.cron
            .create(&params)
            .await
            .map_err(|e| format!("persist poll cron row: {e}"))
    }

    async fn retire_poll_rows(&mut self, keep: &str) -> Result<u64, String> {
        self.cron
            .delete_spool_jobs_for_manifest_except(self.manifest_id, Some(keep))
            .await
            .map_err(|e| format!("cleanup replaced poll cron rows: {e}"))
    }

    async fn cancel(&mut self, batch_id: &str) -> Result<(), String> {
        self.backend()?
            .cancel(batch_id)
            .await
            .map_err(|e| format!("cancel {} ({batch_id}): {e}", self.manifest_id))
    }

    async fn mark_failed(&mut self, error: &str) -> Result<(), String> {
        self.store
            .mark_submit_failed(self.manifest_id, error)
            .await
            .map_err(|e| format!("persist failed submit {}: {e}", self.manifest_id))
    }

    async fn delete_poll_row(&mut self, job_id: &str) -> Result<(), String> {
        self.cron
            .delete(job_id)
            .await
            .map_err(|e| format!("delete poll cron row {job_id}: {e}"))
    }

    async fn persist(
        &mut self,
        state: Option<&SagaState>,
        batch_id: Option<&str>,
    ) -> Result<(), String> {
        let saga_json = state
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("encode saga for {}: {e}", self.manifest_id))?;
        self.store
            .save_saga(self.manifest_id, saga_json.as_deref(), batch_id)
            .await
            .map_err(|e| format!("persist saga for {}: {e}", self.manifest_id))
    }
}
