//! Queued → InProgress submit path.
//!
//! Callers hand over a batch of `(custom_id, TurnRequest)` items plus the
//! backend they want to target. We dispatch to the backend, persist the
//! returned `batch_id` into `spool_jobs`, and hand the `batch_id` back so the
//! caller can wire a cron row (`kind=spool`, `manifest_id=…`) to drive the
//! subsequent poll lifecycle.

use chaos_abi::SpoolRegistry;
use chaos_abi::TurnRequest;
use chaos_vfs::ChaosVfs;

use crate::spool_store::BackendSpoolStore;

/// Submit a batch of turns to `backend_name` and persist a fresh
/// `spool_jobs` row in `InProgress` state.
///
/// Returns the backend-assigned `batch_id`.
pub(crate) async fn submit_manifest(
    registry: &SpoolRegistry,
    store: &BackendSpoolStore,
    manifest_id: &str,
    backend_name: &str,
    items: Vec<(String, TurnRequest)>,
) -> Result<String, String> {
    if items.is_empty() {
        return Err(format!("manifest {manifest_id} has no items to submit"));
    }

    let request_count = u32::try_from(items.len())
        .map_err(|_| format!("too many items for manifest {manifest_id}"))?;
    let custom_ids: Vec<&str> = items.iter().map(|(id, _)| id.as_str()).collect();
    let payload_json = serde_json::to_string(&custom_ids)
        .map_err(|e| format!("serialize custom ids for {manifest_id}: {e}"))?;

    store
        .insert_queued(manifest_id, backend_name, request_count, &payload_json)
        .await
        .map_err(|e| format!("persist queued submit {manifest_id}: {e}"))?;

    let backend = match registry.get(backend_name) {
        Some(backend) => backend,
        None => {
            let msg = format!("no spool backend registered for {backend_name}");
            let _ = store.mark_submit_failed(manifest_id, &msg).await;
            return Err(msg);
        }
    };

    let batch_id = match backend.submit(items).await {
        Ok(batch_id) => batch_id,
        Err(e) => {
            let msg = format!("submit {manifest_id}: {e}");
            let _ = store.mark_submit_failed(manifest_id, &msg).await;
            return Err(msg);
        }
    };

    store
        .insert_submitted(
            manifest_id,
            backend_name,
            &batch_id,
            request_count,
            &payload_json,
        )
        .await
        .map_err(|e| format!("persist submit {manifest_id}: {e}"))?;

    Ok(batch_id)
}

/// Convenience wrapper: build the store from a provider and submit.
pub async fn submit_manifest_from_provider(
    registry: &SpoolRegistry,
    provider: &ChaosVfs,
    manifest_id: &str,
    backend_name: &str,
    items: Vec<(String, TurnRequest)>,
) -> Result<String, String> {
    let store = BackendSpoolStore::from_provider(provider);
    submit_manifest(registry, &store, manifest_id, backend_name, items).await
}

#[cfg(test)]
mod tests;
