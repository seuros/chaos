use anyhow::Context as _;
use chaos_vfs::Vfs;
use pgvector::Vector;
use sqlx::AssertSqlSafe;
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

    /// Build a store on the mounted backend. Recall is pgvector-backed, so a
    /// SQLite mount has nothing to offer it.
    pub fn from_vfs() -> Result<Self, RecallError> {
        Self::from_pool(chaos_vfs::pool().map_err(anyhow::Error::from)?)
    }

    /// Build a store on a backend the caller already holds.
    pub fn from_pool(pool: Vfs) -> Result<Self, RecallError> {
        match pool {
            Vfs::Postgres(pool) => Ok(Self::new(pool)),
            Vfs::Sqlite(_) => Err(RecallError::Backend(anyhow::anyhow!(
                "recall requires a postgres mount"
            ))),
        }
    }

    /// Create extension and table if absent. Idempotent.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&self.pool)
            .await
            .context("create vector extension")?;

        sqlx::query(AssertSqlSafe(format!(
            "CREATE TABLE IF NOT EXISTS recall_docs (
                id          TEXT PRIMARY KEY,
                content     TEXT NOT NULL,
                metadata    JSONB NOT NULL DEFAULT '{{}}',
                embedding   vector({DIM})
            )"
        )))
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
            sqlx::query(AssertSqlSafe(format!("SET hnsw.ef_search = {ef}")))
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

#[cfg(test)]
mod tests {
    use super::DIM;
    use super::PgRecallStore;
    use crate::store::RecallDoc;
    use crate::store::RecallStore;
    use crate::store::SearchRequest;
    use chaos_vfs::ChaosVfs;
    use chaos_vfs::MountConfig;

    const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

    fn unit_vector(seed: f32) -> Vec<f32> {
        let mut embedding = vec![0.0_f32; DIM as usize];
        embedding[0] = seed;
        embedding
    }

    #[tokio::test]
    async fn from_vfs_rejects_a_sqlite_mount() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let vfs = ChaosVfs::from_config(MountConfig::sqlite_home(temp_dir.path()))
            .await
            .expect("open sqlite home");

        let err =
            PgRecallStore::from_pool(vfs.pool()).expect_err("sqlite has no pgvector to offer");
        assert!(
            err.to_string().contains("postgres"),
            "a sqlite mount should say recall needs postgres, got {err}"
        );
    }

    #[tokio::test]
    async fn postgres_mount_indexes_and_searches_a_document() {
        let Ok(database_url) = std::env::var(TEST_DATABASE_URL_ENV) else {
            eprintln!("skipping recall validation; {TEST_DATABASE_URL_ENV} is not set");
            return;
        };

        let config = MountConfig::postgres_url(database_url);
        let vfs = ChaosVfs::from_config(config.clone())
            .await
            .expect("open postgres mount");
        chaos_vfs::set_root(chaos_vfs::mount(config, vfs));

        let store = PgRecallStore::from_vfs().expect("a postgres mount serves recall");
        store.migrate().await.expect("migrate recall schema");

        store
            .index(RecallDoc {
                id: "recall-round-trip".to_string(),
                content: "the kernel mounts one backend at boot".to_string(),
                embedding: unit_vector(1.0),
                metadata: serde_json::json!({"source": "test"}),
            })
            .await
            .expect("index document");

        let results = store
            .search(&SearchRequest::new(unit_vector(1.0), 5))
            .await
            .expect("search recall docs");
        let found = results
            .iter()
            .find(|result| result.id == "recall-round-trip")
            .expect("the indexed document should come back");
        assert_eq!(found.content, "the kernel mounts one backend at boot");
        assert!(found.score > 0.9, "an identical vector should score high");

        store
            .delete("recall-round-trip")
            .await
            .expect("delete recall doc");
    }
}
