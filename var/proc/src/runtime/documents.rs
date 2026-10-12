//! Named user documents that used to be files (`modes/`, `prompts/`,
//! `themes/`, `scripts/`), stored per kind with a per-row revision.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Mode,
    Prompt,
    Theme,
    Script,
}

impl DocumentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::Prompt => "prompt",
            Self::Theme => "theme",
            Self::Script => "script",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDocument {
    pub kind: DocumentKind,
    pub name: String,
    pub source: String,
    pub revision: i64,
    pub enabled: bool,
}

fn kind_from_str(value: &str) -> anyhow::Result<DocumentKind> {
    Ok(match value {
        "mode" => DocumentKind::Mode,
        "prompt" => DocumentKind::Prompt,
        "theme" => DocumentKind::Theme,
        "script" => DocumentKind::Script,
        other => anyhow::bail!("unknown document kind {other}"),
    })
}

macro_rules! documents_backend {
    ($runtime:ty) => {
        impl $runtime {
            async fn list_documents(
                &self,
                kind: DocumentKind,
            ) -> anyhow::Result<Vec<StoredDocument>> {
                let rows = sqlx::query(
                    "SELECT kind, name, source, revision, enabled FROM user_documents \
                     WHERE kind = $1 ORDER BY name",
                )
                .bind(kind.as_str())
                .fetch_all(self.pool())
                .await?;
                rows.iter().map(Self::decode_document).collect()
            }

            async fn get_document(
                &self,
                kind: DocumentKind,
                name: &str,
            ) -> anyhow::Result<Option<StoredDocument>> {
                let row = sqlx::query(
                    "SELECT kind, name, source, revision, enabled FROM user_documents \
                     WHERE kind = $1 AND name = $2",
                )
                .bind(kind.as_str())
                .bind(name)
                .fetch_optional(self.pool())
                .await?;
                row.as_ref().map(Self::decode_document).transpose()
            }

            /// `expected_revision == 0` creates; otherwise the row must still be at
            /// that revision. Returns the new revision.
            async fn put_document(
                &self,
                kind: DocumentKind,
                name: &str,
                source: &str,
                expected_revision: i64,
            ) -> anyhow::Result<i64> {
                anyhow::ensure!(
                    !name.is_empty() && expected_revision >= 0,
                    "invalid document identity"
                );
                let mut tx = self.pool().begin().await?;
                let timestamp = settings::now()?;
                let revision = expected_revision + 1;
                let result = if expected_revision == 0 {
                    sqlx::query(
                        "INSERT INTO user_documents \
                         (kind, name, source, revision, enabled, created_at, updated_at) \
                         VALUES ($1,$2,$3,$4,1,$5,$5) ON CONFLICT (kind, name) DO NOTHING",
                    )
                    .bind(kind.as_str())
                    .bind(name)
                    .bind(source)
                    .bind(revision)
                    .bind(timestamp)
                    .execute(&mut *tx)
                    .await?
                } else {
                    sqlx::query(
                        "UPDATE user_documents SET source = $1, revision = $2, updated_at = $3 \
                         WHERE kind = $4 AND name = $5 AND revision = $6",
                    )
                    .bind(source)
                    .bind(revision)
                    .bind(timestamp)
                    .bind(kind.as_str())
                    .bind(name)
                    .bind(expected_revision)
                    .execute(&mut *tx)
                    .await?
                };
                anyhow::ensure!(
                    result.rows_affected() == 1,
                    "{} revision conflict; reload and retry",
                    kind.as_str()
                );
                Self::record_document_event(&mut tx, kind, "updated", name, timestamp).await?;
                tx.commit().await?;
                Ok(revision)
            }

            async fn set_document_enabled(
                &self,
                kind: DocumentKind,
                name: &str,
                expected_revision: i64,
                enabled: bool,
            ) -> anyhow::Result<i64> {
                let mut tx = self.pool().begin().await?;
                let timestamp = settings::now()?;
                let revision = expected_revision + 1;
                let result = sqlx::query(
                    "UPDATE user_documents SET enabled = $1, revision = $2, updated_at = $3 \
                     WHERE kind = $4 AND name = $5 AND revision = $6",
                )
                .bind(i64::from(enabled))
                .bind(revision)
                .bind(timestamp)
                .bind(kind.as_str())
                .bind(name)
                .bind(expected_revision)
                .execute(&mut *tx)
                .await?;
                anyhow::ensure!(
                    result.rows_affected() == 1,
                    "{} revision conflict; reload and retry",
                    kind.as_str()
                );
                Self::record_document_event(&mut tx, kind, "updated", name, timestamp).await?;
                tx.commit().await?;
                Ok(revision)
            }

            async fn delete_document(
                &self,
                kind: DocumentKind,
                name: &str,
                expected_revision: i64,
            ) -> anyhow::Result<()> {
                let mut tx = self.pool().begin().await?;
                let result = sqlx::query(
                    "DELETE FROM user_documents WHERE kind = $1 AND name = $2 AND revision = $3",
                )
                .bind(kind.as_str())
                .bind(name)
                .bind(expected_revision)
                .execute(&mut *tx)
                .await?;
                anyhow::ensure!(
                    result.rows_affected() == 1,
                    "{} revision conflict; reload and retry",
                    kind.as_str()
                );
                Self::record_document_event(&mut tx, kind, "deleted", name, settings::now()?)
                    .await?;
                tx.commit().await?;
                Ok(())
            }

            async fn record_document_event<'c>(
                tx: &mut sqlx::Transaction<'c, <Self as DocumentsBackend>::Db>,
                kind: DocumentKind,
                action: &str,
                name: &str,
                timestamp: i64,
            ) -> anyhow::Result<()> {
                sqlx::query(
                    "INSERT INTO configuration_events (id, event, subject, created_at) \
                     VALUES ($1,$2,$3,$4)",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(format!("{}_{action}", kind.as_str()))
                .bind(name)
                .bind(timestamp)
                .execute(&mut **tx)
                .await?;
                Ok(())
            }

            fn decode_document<R: sqlx::Row>(row: &R) -> anyhow::Result<StoredDocument>
            where
                for<'a> &'a str: sqlx::ColumnIndex<R>,
                for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
                for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
            {
                Ok(StoredDocument {
                    kind: kind_from_str(&row.try_get::<String, _>("kind")?)?,
                    name: row.try_get("name")?,
                    source: row.try_get("source")?,
                    revision: row.try_get("revision")?,
                    enabled: row.try_get::<i64, _>("enabled")? != 0,
                })
            }
        }
    };
}

/// Names each runtime's database type for the shared transaction helper.
trait DocumentsBackend {
    type Db: sqlx::Database;
}

impl DocumentsBackend for StateRuntime {
    type Db = sqlx::Sqlite;
}

impl DocumentsBackend for PostgresRuntime {
    type Db = sqlx::Postgres;
}

documents_backend!(StateRuntime);
documents_backend!(PostgresRuntime);

impl RuntimeDbHandle {
    chaos_dispatch::backend_dispatch! {
        pub async fn list_documents(
            &self,
            kind: DocumentKind,
        ) -> anyhow::Result<Vec<StoredDocument>>;
        pub async fn get_document(
            &self,
            kind: DocumentKind,
            name: &str,
        ) -> anyhow::Result<Option<StoredDocument>>;
        pub async fn put_document(
            &self,
            kind: DocumentKind,
            name: &str,
            source: &str,
            expected_revision: i64,
        ) -> anyhow::Result<i64>;
        pub async fn set_document_enabled(
            &self,
            kind: DocumentKind,
            name: &str,
            expected_revision: i64,
            enabled: bool,
        ) -> anyhow::Result<i64>;
        pub async fn delete_document(
            &self,
            kind: DocumentKind,
            name: &str,
            expected_revision: i64,
        ) -> anyhow::Result<()>;
    }
}

#[cfg(test)]
mod tests;
