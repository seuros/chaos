use anyhow::Context as _;
use pgvector::Vector;
use sqlx::PgPool;
use tracing::{debug, instrument};

use crate::store::{RecallDoc, RecallError, RecallStore, SearchRequest, SearchResult};

/// Dimension of embeddings stored in this table.
/// Must match the model used by the indexer (potion-base-8M → 256).
const DIM: i32 = 256;

/// pgvector-backed recall store.
///
/// Expects the `vector` extension and the `recall_docs` table to exist.
/// Call [`PgRecallStore::migrate`] once during startup.
#[derive(Debug, Clone)]
pub struct PgRecallStore {
    pool: PgPool,
}

impl PgRecallStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create extension and table if absent. Idempotent.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&self.pool)
            .await
            .context("create vector extension")?;

        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS recall_docs (
                id          TEXT PRIMARY KEY,
                content     TEXT NOT NULL,
                metadata    JSONB NOT NULL DEFAULT '{{}}',
                embedding   vector({DIM})
            )"
        ))
        .execute(&self.pool)
        .await
        .context("create recall_docs table")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS recall_docs_hnsw
             ON recall_docs USING hnsw (embedding vector_cosine_ops)",
        )
        .execute(&self.pool)
        .await
        .context("create hnsw index")?;

        Ok(())
    }

    fn check_dim(&self, v: &[f32]) -> Result<(), RecallError> {
        if v.len() != DIM as usize {
            return Err(RecallError::DimMismatch {
                expected: DIM as usize,
                got: v.len(),
            });
        }
        Ok(())
    }
}

impl RecallStore for PgRecallStore {
    #[instrument(skip(self, doc))]
    async fn index(&self, doc: RecallDoc) -> Result<(), RecallError> {
        self.check_dim(&doc.embedding)?;
        let vec = Vector::from(doc.embedding);
        sqlx::query(
            "INSERT INTO recall_docs (id, content, metadata, embedding)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE
               SET content   = EXCLUDED.content,
                   metadata  = EXCLUDED.metadata,
                   embedding = EXCLUDED.embedding",
        )
        .bind(&doc.id)
        .bind(&doc.content)
        .bind(&doc.metadata)
        .bind(vec)
        .execute(&self.pool)
        .await
        .context("upsert recall doc")
        .map_err(RecallError::Backend)?;

        Ok(())
    }

    #[instrument(skip(self, docs), fields(n = docs.len()))]
    async fn index_batch(&self, docs: Vec<RecallDoc>) -> Result<(), RecallError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("begin transaction")
            .map_err(RecallError::Backend)?;

        for doc in docs {
            self.check_dim(&doc.embedding)?;
            let vec = Vector::from(doc.embedding);
            sqlx::query(
                "INSERT INTO recall_docs (id, content, metadata, embedding)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (id) DO UPDATE
                   SET content   = EXCLUDED.content,
                       metadata  = EXCLUDED.metadata,
                       embedding = EXCLUDED.embedding",
            )
            .bind(&doc.id)
            .bind(&doc.content)
            .bind(&doc.metadata)
            .bind(vec)
            .execute(&mut *tx)
            .await
            .context("upsert in batch")
            .map_err(RecallError::Backend)?;
        }

        tx.commit()
            .await
            .context("commit batch")
            .map_err(RecallError::Backend)?;

        Ok(())
    }

    #[instrument(skip(self, req), fields(limit = req.limit))]
    async fn search(&self, req: &SearchRequest) -> Result<Vec<SearchResult>, RecallError> {
        self.check_dim(&req.query_vec)?;

        if let Some(ef) = req.ef_search {
            sqlx::query(&format!("SET hnsw.ef_search = {ef}"))
                .execute(&self.pool)
                .await
                .context("set ef_search")
                .map_err(RecallError::Backend)?;
        }

        let vec = Vector::from(req.query_vec.clone());

        let rows: Vec<(String, String, serde_json::Value, f32)> = sqlx::query_as(
            "SELECT id, content, metadata,
                    (1 - (embedding <=> $1))::float4 AS score
             FROM recall_docs
             ORDER BY embedding <=> $1
             LIMIT $2",
        )
        .bind(vec)
        .bind(req.limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("vector search")
        .map_err(RecallError::Backend)?;

        let results = rows
            .into_iter()
            .filter(|(_, _, _, score)| req.min_score.is_none_or(|min| *score >= min))
            .map(|(id, content, metadata, score)| SearchResult {
                id,
                score,
                content,
                metadata,
            })
            .collect();

        debug!("search returned {} results", {
            let r: &Vec<SearchResult> = &results;
            r.len()
        });
        Ok(results)
    }

    #[instrument(skip(self))]
    async fn delete(&self, id: &str) -> Result<(), RecallError> {
        sqlx::query("DELETE FROM recall_docs WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete recall doc")
            .map_err(RecallError::Backend)?;
        Ok(())
    }
}
