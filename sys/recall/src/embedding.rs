use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use model2vec_rs::model::StaticModel;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::store::{RecallError, normalize_embedding};

/// Implementations must keep their model identity stable for their lifetime.
pub trait EmbeddingProvider: Send + Sync + 'static {
    fn fingerprint(&self) -> &str;
    fn embed(
        &self,
        text: &str,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<Vec<f32>, RecallError>> + Send;
}

/// Pure Rust CPU inference. Neither Hub support nor the CLI is compiled in.
pub struct LocalEmbedder {
    model: Arc<StaticModel>,
    fingerprint: String,
    permits: Arc<Semaphore>,
}

impl LocalEmbedder {
    pub async fn load(
        directory: PathBuf,
        concurrency: usize,
        cancellation: CancellationToken,
    ) -> Result<Self, RecallError> {
        if concurrency == 0 {
            return Err(RecallError::InvalidInput(
                "embedding concurrency must be nonzero",
            ));
        }
        if cancellation.is_cancelled() {
            return Err(RecallError::Cancelled);
        }
        let mut task =
            tokio::task::spawn_blocking(move || Self::load_sync(&directory, concurrency));
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                // The kernel keeps initialization ownership until cancellation
                // drains. Do not detach a still-loading model and permit a retry
                // to allocate another one on top of it.
                let _ = task.await;
                Err(RecallError::Cancelled)
            },
            loaded = &mut task => loaded
                .map_err(|error| RecallError::ModelLoad(error.into()))?,
        }
    }

    fn load_sync(directory: &Path, concurrency: usize) -> Result<Self, RecallError> {
        let load = || -> anyhow::Result<Self> {
            let mut fingerprint = Sha256::new();
            // Include the inference contract, not just weights.
            fingerprint.update(b"chaos-recall/model2vec-0.3.0/l2/max-tokens-512/v1");
            let mut artifacts = Vec::new();
            for name in ["tokenizer.json", "model.safetensors", "config.json"] {
                let path = directory.join(name);
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("read local model artifact {}", path.display()))?;
                fingerprint.update(name.as_bytes());
                fingerprint.update((bytes.len() as u64).to_le_bytes());
                fingerprint.update(&bytes);
                artifacts.push(bytes);
            }
            let model =
                StaticModel::from_bytes(&artifacts[0], &artifacts[1], &artifacts[2], Some(true))
                    .context("load local potion-base-8M artifacts")?;
            normalize_embedding(model.encode_single("recall model validation"))
                .context("model must produce finite nonzero 256-dimensional embeddings")?;
            Ok(Self {
                model: Arc::new(model),
                fingerprint: fingerprint
                    .finalize()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
                permits: Arc::new(Semaphore::new(concurrency)),
            })
        };
        load().map_err(RecallError::ModelLoad)
    }
}

impl EmbeddingProvider for LocalEmbedder {
    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    async fn embed(
        &self,
        text: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<f32>, RecallError> {
        let permit = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(RecallError::Cancelled),
            permit = Arc::clone(&self.permits).acquire_owned() => permit
                .map_err(|_| RecallError::Workflow("embedding semaphore closed".into()))?,
        };
        let model = Arc::clone(&self.model);
        let text = text.to_owned();
        let task = tokio::task::spawn_blocking(move || {
            // Blocking CPU work cannot be aborted. Keep the concurrency permit
            // until it finishes, even if its async caller is cancelled.
            let _permit = permit;
            normalize_embedding(model.encode_single(&text))
        });
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(RecallError::Cancelled),
            result = task => result
                .map_err(|error| RecallError::Workflow(format!("embedding task failed: {error}")))?,
        }
    }
}
