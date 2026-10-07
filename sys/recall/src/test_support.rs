use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use time::OffsetDateTime;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::*;

pub type Documents = BTreeMap<(RecallScope, String), (RecallDoc, MemoryHandle)>;

#[derive(Clone, Default)]
pub struct Store {
    pub docs: Arc<Mutex<Documents>>,
    pub fail_semantic: Arc<AtomicBool>,
    pub stalled: Arc<AtomicBool>,
    pub active: Arc<AtomicUsize>,
    receipts: Arc<Mutex<BTreeMap<String, Receipt>>>,
    clock: Arc<Clock>,
}

struct Clock {
    timestamp: OffsetDateTime,
    instant: Instant,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            timestamp: OffsetDateTime::now_utc(),
            instant: Instant::now(),
        }
    }
}

struct Receipt {
    owner: String,
    generation: String,
    expires: Instant,
    selections: BTreeMap<(RecallScope, String), (String, Option<f64>)>,
}

struct Active(Arc<AtomicUsize>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Store {
    fn now(&self) -> OffsetDateTime {
        self.clock.timestamp + self.clock.instant.elapsed()
    }

    async fn retrieve(&self, filter: &RetrievalFilter) -> Result<Vec<SearchResult>, RecallError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _guard = Active(self.active.clone());
        if self.stalled.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(self
            .docs
            .lock()
            .await
            .values()
            .filter(|(doc, handle)| {
                (doc.scope == filter.scope
                    || (filter.include_global && doc.scope == RecallScope::Global))
                    && (!filter.automatic_only || handle.options.automatic)
            })
            .take(filter.limit)
            .map(|(doc, handle)| SearchResult {
                scope: doc.scope.clone(),
                id: doc.id.clone(),
                score: 1.0,
                content: doc.content.clone(),
                metadata: doc.metadata.clone(),
                handle: handle.clone(),
            })
            .collect())
    }
}

impl RecallStore for Store {
    async fn index(&self, doc: RecallDoc) -> Result<(), RecallError> {
        self.index_memory(doc, MemoryOptions::default()).await
    }

    async fn index_batch(&self, docs: Vec<RecallDoc>) -> Result<(), RecallError> {
        for doc in &docs {
            doc.validate()?;
        }
        for doc in docs {
            self.index(doc).await?;
        }
        Ok(())
    }

    async fn search(&self, request: &SearchRequest) -> Result<Vec<SearchResult>, RecallError> {
        if self.fail_semantic.load(Ordering::SeqCst) {
            return Err(RecallError::Backend(anyhow::anyhow!(
                "private SQL diagnostic"
            )));
        }
        self.retrieve(&request.filter).await
    }

    async fn lexical_search(
        &self,
        _: &str,
        filter: &RetrievalFilter,
    ) -> Result<Vec<SearchResult>, RecallError> {
        self.retrieve(filter).await
    }

    async fn delete(&self, scope: &RecallScope, id: &str) -> Result<(), RecallError> {
        self.docs.lock().await.remove(&(scope.clone(), id.into()));
        Ok(())
    }
}

impl MemoryStore for Store {
    async fn index_memory(
        &self,
        doc: RecallDoc,
        options: MemoryOptions,
    ) -> Result<(), RecallError> {
        doc.validate()?;
        options.validate()?;
        let mut docs = self.docs.lock().await;
        let key = (doc.scope.clone(), doc.id.clone());
        let now = self.now();
        let timestamps = match docs.get(&key) {
            Some((previous, handle)) => MemoryTimestamps {
                updated_at: if previous.content == doc.content {
                    handle.timestamps.updated_at
                } else {
                    now.max(handle.timestamps.updated_at)
                },
                ..handle.timestamps
            },
            None => MemoryTimestamps {
                created_at: now,
                updated_at: now,
                last_used_at: None,
            },
        };
        let handle = MemoryHandle {
            revision: uuid::Uuid::new_v4().to_string(),
            options,
            timestamps,
        };
        docs.insert(key, (doc, handle));
        Ok(())
    }

    async fn issue_receipt(
        &self,
        owner: &str,
        generation: &str,
        selections: &[MemorySelection],
    ) -> Result<UseReceipt, RecallError> {
        if owner.is_empty() || selections.is_empty() || selections.len() > 50 {
            return Err(RecallError::InvalidReceipt);
        }
        let docs = self.docs.lock().await;
        let mut entries = BTreeMap::new();
        for selection in selections {
            let key = (selection.scope.clone(), selection.id.clone());
            let (_, handle) = docs.get(&key).ok_or(RecallError::InvalidReceipt)?;
            if handle.revision != selection.revision {
                return Err(RecallError::InvalidReceipt);
            }
            entries.insert(key, (selection.revision.clone(), None));
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.receipts.lock().await.insert(
            id.clone(),
            Receipt {
                owner: owner.into(),
                generation: generation.into(),
                expires: Instant::now() + Duration::from_secs(3600),
                selections: entries,
            },
        );
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(anyhow::Error::from)?
            .as_secs() as i64
            + 3600;
        Ok(UseReceipt { id, expires_at })
    }

    async fn recalled_memory(
        &self,
        owner: &str,
        generation: &str,
        receipt: &str,
        scope: &RecallScope,
        id: &str,
    ) -> Result<MemoryHandle, RecallError> {
        let docs = self.docs.lock().await;
        let receipts = self.receipts.lock().await;
        let receipt = receipts.get(receipt).ok_or(RecallError::InvalidReceipt)?;
        let key = (scope.clone(), id.into());
        let (revision, _) = receipt
            .selections
            .get(&key)
            .ok_or(RecallError::InvalidReceipt)?;
        let (_, handle) = docs.get(&key).ok_or(RecallError::InvalidReceipt)?;
        if receipt.owner != owner
            || receipt.generation != generation
            || receipt.expires <= Instant::now()
            || revision != &handle.revision
        {
            return Err(RecallError::InvalidReceipt);
        }
        Ok(handle.clone())
    }

    async fn commit_use(
        &self,
        owner: &str,
        generation: &str,
        receipt: &str,
        scope: &RecallScope,
        id: &str,
    ) -> Result<UseOutcome, RecallError> {
        if matches!(scope, RecallScope::Global) {
            return Err(RecallError::InvalidReceipt);
        }
        let mut docs = self.docs.lock().await;
        let mut receipts = self.receipts.lock().await;
        let receipt = receipts
            .get_mut(receipt)
            .ok_or(RecallError::InvalidReceipt)?;
        let key = (scope.clone(), id.into());
        let (revision, applied) = receipt
            .selections
            .get_mut(&key)
            .ok_or(RecallError::InvalidReceipt)?;
        let (_, handle) = docs.get_mut(&key).ok_or(RecallError::InvalidReceipt)?;
        if receipt.owner != owner
            || receipt.generation != generation
            || receipt.expires <= Instant::now()
            || revision != &handle.revision
        {
            return Err(RecallError::InvalidReceipt);
        }
        if let Some(charge) = applied {
            return Ok(UseOutcome {
                charge: *charge,
                applied: false,
            });
        }
        handle.options.charge = (handle.options.charge + 0.02).min(1.0);
        let now = self.now();
        handle.timestamps.last_used_at = Some(
            handle
                .timestamps
                .last_used_at
                .map_or(now, |previous| previous.max(now)),
        );
        *applied = Some(handle.options.charge);
        Ok(UseOutcome {
            charge: handle.options.charge,
            applied: true,
        })
    }
}

pub struct Embedder;

impl EmbeddingProvider for Embedder {
    fn fingerprint(&self) -> &str {
        "kernel-test"
    }

    async fn embed(&self, _: &str, _: CancellationToken) -> Result<Vec<f32>, RecallError> {
        Ok(vec![1.0; 256])
    }
}
