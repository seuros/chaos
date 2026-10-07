use std::sync::LazyLock;

use chaos_vfs::Vfs;
use pgvector::Vector;
use sqlx::PgPool;
use sqlx::migrate::Migrator;
use time::OffsetDateTime;
use tracing::instrument;

use crate::store::{
    RecallDoc, RecallError, RecallScope, RecallStore, RetrievalFilter, SearchRequest, SearchResult,
    validate_embedding,
};

static MIGRATOR: LazyLock<Migrator> = LazyLock::new(|| {
    let mut migrator = sqlx::migrate!("./db/migrate/postgres");
    migrator.dangerous_set_table_name("_recall_migrations");
    migrator
});

type Row = (
    String,
    String,
    String,
    String,
    serde_json::Value,
    f32,
    String,
    serde_json::Value,
    bool,
    f64,
    OffsetDateTime,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

#[derive(Debug, Clone)]
pub struct PgRecallStore {
    pool: PgPool,
}

impl PgRecallStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_vfs() -> Result<Self, RecallError> {
        Self::from_pool(chaos_vfs::pool().map_err(anyhow::Error::from)?)
    }

    pub fn from_pool(pool: Vfs) -> Result<Self, RecallError> {
        match pool {
            Vfs::Postgres(pool) => Ok(Self::new(pool)),
            Vfs::Sqlite(_) => Err(RecallError::Backend(anyhow::anyhow!(
                "recall requires a postgres mount"
            ))),
        }
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    pub async fn bind_model(&self, fingerprint: &str) -> Result<(), RecallError> {
        if fingerprint.is_empty() {
            return Err(RecallError::InvalidInput("model fingerprint is empty"));
        }
        let bound: bool = sqlx::query_scalar("SELECT recall_bind_model($1)")
            .bind(fingerprint)
            .fetch_one(&self.pool)
            .await
            .map_err(database_error)?;
        if !bound {
            return Err(RecallError::ModelMismatch);
        }
        Ok(())
    }
}

fn database_error(error: sqlx::Error) -> RecallError {
    let code = error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    match code.as_deref() {
        Some("RC001") => RecallError::InvalidReceipt,
        Some("RC002") => RecallError::ModelMismatch,
        _ => RecallError::Backend(error.into()),
    }
}

impl RecallStore for PgRecallStore {
    #[instrument(skip_all)]
    async fn index(&self, doc: RecallDoc) -> Result<(), RecallError> {
        self.index_batch(vec![doc]).await
    }

    #[instrument(skip_all, fields(count = docs.len()))]
    async fn index_batch(&self, mut docs: Vec<RecallDoc>) -> Result<(), RecallError> {
        for doc in &docs {
            doc.validate()?;
        }
        docs.sort_by(|left, right| (&left.scope, &left.id).cmp(&(&right.scope, &right.id)));
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        for doc in docs {
            memory::write_doc(&mut tx, doc, crate::MemoryOptions::default()).await?;
        }
        tx.commit().await.map_err(database_error)?;
        Ok(())
    }

    #[instrument(skip_all, fields(limit = req.filter.limit))]
    async fn search(&self, req: &SearchRequest) -> Result<Vec<SearchResult>, RecallError> {
        req.filter.validate()?;
        validate_embedding(&req.query_vec)?;
        let (kind, scope_id) = req.filter.scope.parts();
        let rows: Vec<Row> =
            sqlx::query_as("SELECT * FROM recall_search($1, $2, $3, $4, $5, $6, $7)")
                .bind(Vector::from(req.query_vec.clone()))
                .bind(kind)
                .bind(scope_id)
                .bind(req.filter.include_global)
                .bind(&req.filter.model_fingerprint)
                .bind(req.filter.limit as i64)
                .bind(req.filter.automatic_only)
                .fetch_all(&self.pool)
                .await
                .map_err(database_error)?;
        decode_rows(rows)
    }

    #[instrument(skip_all, fields(limit = filter.limit))]
    async fn lexical_search(
        &self,
        query: &str,
        filter: &RetrievalFilter,
    ) -> Result<Vec<SearchResult>, RecallError> {
        filter.validate()?;
        let (kind, scope_id) = filter.scope.parts();
        let rows: Vec<Row> =
            sqlx::query_as("SELECT * FROM recall_lexical_search($1, $2, $3, $4, $5, $6, $7)")
                .bind(query)
                .bind(kind)
                .bind(scope_id)
                .bind(filter.include_global)
                .bind(&filter.model_fingerprint)
                .bind(filter.limit as i64)
                .bind(filter.automatic_only)
                .fetch_all(&self.pool)
                .await
                .map_err(database_error)?;
        decode_rows(rows)
    }

    #[instrument(skip_all)]
    async fn delete(&self, scope: &RecallScope, id: &str) -> Result<(), RecallError> {
        scope.validate()?;
        if id.trim().is_empty() {
            return Err(RecallError::InvalidInput("document ID is empty"));
        }
        let (kind, scope_id) = scope.parts();
        sqlx::query("SELECT recall_delete($1, $2, $3)")
            .bind(kind)
            .bind(scope_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }
}

fn decode_rows(rows: Vec<Row>) -> Result<Vec<SearchResult>, RecallError> {
    rows.into_iter()
        .map(
            |(
                kind,
                scope_id,
                id,
                content,
                metadata,
                score,
                revision,
                sources,
                automatic,
                charge,
                created_at,
                updated_at,
                last_used_at,
            )| {
                let options = crate::MemoryOptions {
                    sources: serde_json::from_value(sources).map_err(anyhow::Error::from)?,
                    automatic,
                    charge,
                };
                options.validate()?;
                Ok(SearchResult {
                    scope: RecallScope::from_parts(kind, scope_id)?,
                    id,
                    score,
                    content,
                    metadata,
                    handle: crate::MemoryHandle {
                        revision,
                        options,
                        timestamps: crate::MemoryTimestamps {
                            created_at,
                            updated_at,
                            last_used_at,
                        },
                    },
                })
            },
        )
        .collect()
}

mod memory;

#[cfg(test)]
pub(crate) mod tests;
