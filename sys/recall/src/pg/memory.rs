use super::*;
use crate::{
    MemoryHandle, MemoryOptions, MemorySelection, MemoryStore, MemoryTimestamps, UseOutcome,
    UseReceipt,
};

type MemoryRow = (
    String,
    serde_json::Value,
    bool,
    f64,
    OffsetDateTime,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

pub(super) async fn write_doc(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    doc: RecallDoc,
    options: MemoryOptions,
) -> Result<(), RecallError> {
    doc.validate()?;
    options.validate()?;
    let (kind, scope_id) = doc.scope.parts();
    let stored: bool =
        sqlx::query_scalar("SELECT recall_store($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
            .bind(kind)
            .bind(scope_id)
            .bind(&doc.id)
            .bind(&doc.content)
            .bind(&doc.metadata)
            .bind(Vector::from(doc.embedding))
            .bind(&doc.model_fingerprint)
            .bind(serde_json::to_value(options.sources).map_err(anyhow::Error::from)?)
            .bind(options.automatic)
            .bind(options.charge)
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?;
    if !stored {
        return Err(RecallError::ModelMismatch);
    }
    Ok(())
}

impl MemoryStore for PgRecallStore {
    async fn index_memory(
        &self,
        doc: RecallDoc,
        options: MemoryOptions,
    ) -> Result<(), RecallError> {
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        write_doc(&mut tx, doc, options).await?;
        tx.commit().await.map_err(database_error)?;
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
        let (id, expires_at): (String, i64) =
            sqlx::query_as("SELECT * FROM recall_issue_receipt($1, $2, $3)")
                .bind(owner)
                .bind(generation)
                .bind(serde_json::to_value(selections).map_err(anyhow::Error::from)?)
                .fetch_one(&self.pool)
                .await
                .map_err(database_error)?;
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
        let (kind, scope_id) = scope.parts();
        let row: Option<MemoryRow> =
            sqlx::query_as("SELECT * FROM recall_recalled_memory($1, $2, $3, $4, $5, $6)")
                .bind(owner)
                .bind(generation)
                .bind(receipt)
                .bind(kind)
                .bind(scope_id)
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(database_error)?;
        let (revision, sources, automatic, charge, created_at, updated_at, last_used_at) =
            row.ok_or(RecallError::InvalidReceipt)?;
        let options = MemoryOptions {
            sources: serde_json::from_value(sources).map_err(anyhow::Error::from)?,
            automatic,
            charge,
        };
        options.validate()?;
        Ok(MemoryHandle {
            revision,
            options,
            timestamps: MemoryTimestamps {
                created_at,
                updated_at,
                last_used_at,
            },
        })
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
        let (kind, scope_id) = scope.parts();
        let (charge, applied): (f64, bool) =
            sqlx::query_as("SELECT * FROM recall_commit_use($1, $2, $3, $4, $5, $6)")
                .bind(owner)
                .bind(generation)
                .bind(receipt)
                .bind(kind)
                .bind(scope_id)
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(database_error)?;
        Ok(UseOutcome { charge, applied })
    }
}
