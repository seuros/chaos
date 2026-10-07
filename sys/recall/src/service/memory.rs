use super::*;
use crate::{MemoryHandle, MemoryOptions, MemorySelection, MemoryStore, UseOutcome};

impl<S: MemoryStore, E: EmbeddingProvider> RecallService<S, E> {
    pub async fn store_memory(
        &self,
        scope: RecallScope,
        id: String,
        content: String,
        metadata: serde_json::Value,
        options: MemoryOptions,
        cancellation: CancellationToken,
    ) -> Result<(), RecallError> {
        validate_document(&scope, &id, &content)?;
        options.validate()?;
        let embedding = embed_text(self.embedder.as_ref(), &content, cancellation.clone()).await?;
        let doc = RecallDoc {
            scope,
            id,
            content,
            metadata,
            embedding,
            model_fingerprint: self.embedder.fingerprint().to_owned(),
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(RecallError::Cancelled),
            result = self.store.index_memory(doc, options) => result,
        }
    }

    pub async fn attach_receipt(
        &self,
        owner: &str,
        response: &mut RecallResponse,
    ) -> Result<(), RecallError> {
        if owner.is_empty() || response.matches.len() > 50 {
            return Err(RecallError::InvalidInput(
                "invalid receipt owner or selection",
            ));
        }
        if response.matches.is_empty() {
            response.receipt = None;
            return Ok(());
        }
        let selections: Vec<_> = response
            .matches
            .iter()
            .map(|memory| MemorySelection {
                scope: memory.scope.clone(),
                id: memory.id.clone(),
                revision: memory.handle.revision.clone(),
            })
            .collect();
        response.receipt = Some(
            tokio::time::timeout(
                self.timeout,
                self.store
                    .issue_receipt(owner, &self.generation, &selections),
            )
            .await
            .map_err(|_| RecallError::Timeout)??,
        );
        Ok(())
    }

    pub async fn recalled_memory(
        &self,
        owner: &str,
        receipt: &str,
        scope: &RecallScope,
        id: &str,
    ) -> Result<MemoryHandle, RecallError> {
        tokio::time::timeout(
            self.timeout,
            self.store
                .recalled_memory(owner, &self.generation, receipt, scope, id),
        )
        .await
        .map_err(|_| RecallError::Timeout)?
    }

    pub async fn use_memory(
        &self,
        owner: &str,
        receipt: &str,
        scope: &RecallScope,
        id: &str,
    ) -> Result<UseOutcome, RecallError> {
        if matches!(scope, RecallScope::Global) {
            return Err(RecallError::InvalidReceipt);
        }
        tokio::time::timeout(
            self.timeout,
            self.store
                .commit_use(owner, &self.generation, receipt, scope, id),
        )
        .await
        .map_err(|_| RecallError::Timeout)?
    }
}

#[cfg(test)]
mod tests;
