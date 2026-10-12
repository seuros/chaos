//! Agent role definitions: the same TOML or Markdown documents that used to
//! live under `agents/`, stored by name with a per-row revision.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRoleRecord {
    pub name: String,
    /// `toml` for a whole-file TOML document, `md` for Markdown with front matter.
    pub format: String,
    pub source: String,
    /// Directory relative paths inside the document resolve against.
    pub base_dir: Option<String>,
    pub revision: i64,
    pub enabled: bool,
}

macro_rules! agent_roles_backend {
    ($runtime:ty) => {
        impl $runtime {
            async fn list_agent_roles(&self) -> anyhow::Result<Vec<AgentRoleRecord>> {
                let rows = sqlx::query(
                    "SELECT name, format, source, base_dir, revision, enabled \
                     FROM agent_roles ORDER BY name",
                )
                .fetch_all(self.pool())
                .await?;
                rows.iter().map(Self::decode_agent_role).collect()
            }

            async fn get_agent_role(&self, name: &str) -> anyhow::Result<Option<AgentRoleRecord>> {
                let row = sqlx::query(
                    "SELECT name, format, source, base_dir, revision, enabled \
                     FROM agent_roles WHERE name = $1",
                )
                .bind(name)
                .fetch_optional(self.pool())
                .await?;
                row.as_ref().map(Self::decode_agent_role).transpose()
            }

            /// `expected_revision == 0` creates; otherwise the row must still be at
            /// that revision. Returns the new revision.
            async fn put_agent_role(
                &self,
                name: &str,
                format: &str,
                source: &str,
                base_dir: Option<&str>,
                expected_revision: i64,
            ) -> anyhow::Result<i64> {
                anyhow::ensure!(
                    !name.is_empty() && expected_revision >= 0,
                    "invalid role identity"
                );
                anyhow::ensure!(
                    matches!(format, "toml" | "md"),
                    "role format must be toml or md"
                );
                let mut tx = self.pool().begin().await?;
                let timestamp = settings::now()?;
                let revision = expected_revision + 1;
                let result = if expected_revision == 0 {
                    sqlx::query(
                        "INSERT INTO agent_roles (name, format, source, base_dir, \
                         revision, enabled, created_at, updated_at) \
                         VALUES ($1,$2,$3,$4,$5,1,$6,$6) ON CONFLICT (name) DO NOTHING",
                    )
                    .bind(name)
                    .bind(format)
                    .bind(source)
                    .bind(base_dir)
                    .bind(revision)
                    .bind(timestamp)
                    .execute(&mut *tx)
                    .await?
                } else {
                    sqlx::query(
                        "UPDATE agent_roles SET format = $1, source = $2, base_dir = $3, \
                         revision = $4, updated_at = $5 WHERE name = $6 AND revision = $7",
                    )
                    .bind(format)
                    .bind(source)
                    .bind(base_dir)
                    .bind(revision)
                    .bind(timestamp)
                    .bind(name)
                    .bind(expected_revision)
                    .execute(&mut *tx)
                    .await?
                };
                anyhow::ensure!(
                    result.rows_affected() == 1,
                    "agent role revision conflict; reload and retry"
                );
                sqlx::query(
                    "INSERT INTO configuration_events (id, event, subject, created_at) \
                     VALUES ($1,'agent_role_updated',$2,$3)",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(name)
                .bind(timestamp)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(revision)
            }

            async fn set_agent_role_enabled(
                &self,
                name: &str,
                expected_revision: i64,
                enabled: bool,
            ) -> anyhow::Result<i64> {
                let mut tx = self.pool().begin().await?;
                let timestamp = settings::now()?;
                let revision = expected_revision + 1;
                let result = sqlx::query(
                    "UPDATE agent_roles SET enabled = $1, revision = $2, updated_at = $3 \
                     WHERE name = $4 AND revision = $5",
                )
                .bind(i64::from(enabled))
                .bind(revision)
                .bind(timestamp)
                .bind(name)
                .bind(expected_revision)
                .execute(&mut *tx)
                .await?;
                anyhow::ensure!(
                    result.rows_affected() == 1,
                    "agent role revision conflict; reload and retry"
                );
                sqlx::query(
                    "INSERT INTO configuration_events (id, event, subject, created_at) \
                     VALUES ($1,'agent_role_updated',$2,$3)",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(name)
                .bind(timestamp)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(revision)
            }

            async fn delete_agent_role(
                &self,
                name: &str,
                expected_revision: i64,
            ) -> anyhow::Result<()> {
                let mut tx = self.pool().begin().await?;
                let result =
                    sqlx::query("DELETE FROM agent_roles WHERE name = $1 AND revision = $2")
                        .bind(name)
                        .bind(expected_revision)
                        .execute(&mut *tx)
                        .await?;
                anyhow::ensure!(
                    result.rows_affected() == 1,
                    "agent role revision conflict; reload and retry"
                );
                sqlx::query(
                    "INSERT INTO configuration_events (id, event, subject, created_at) \
                     VALUES ($1,'agent_role_deleted',$2,$3)",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(name)
                .bind(settings::now()?)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(())
            }

            fn decode_agent_role<R: sqlx::Row>(row: &R) -> anyhow::Result<AgentRoleRecord>
            where
                for<'a> &'a str: sqlx::ColumnIndex<R>,
                for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
                for<'r> Option<String>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
                for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
            {
                Ok(AgentRoleRecord {
                    name: row.try_get("name")?,
                    format: row.try_get("format")?,
                    source: row.try_get("source")?,
                    base_dir: row.try_get("base_dir")?,
                    revision: row.try_get("revision")?,
                    enabled: row.try_get::<i64, _>("enabled")? != 0,
                })
            }
        }
    };
}

agent_roles_backend!(StateRuntime);
agent_roles_backend!(PostgresRuntime);

impl RuntimeDbHandle {
    chaos_dispatch::backend_dispatch! {
        pub async fn list_agent_roles(&self) -> anyhow::Result<Vec<AgentRoleRecord>>;
        pub async fn get_agent_role(&self, name: &str) -> anyhow::Result<Option<AgentRoleRecord>>;
        pub async fn put_agent_role(
            &self,
            name: &str,
            format: &str,
            source: &str,
            base_dir: Option<&str>,
            expected_revision: i64,
        ) -> anyhow::Result<i64>;
        pub async fn set_agent_role_enabled(
            &self,
            name: &str,
            expected_revision: i64,
            enabled: bool,
        ) -> anyhow::Result<i64>;
        pub async fn delete_agent_role(
            &self,
            name: &str,
            expected_revision: i64,
        ) -> anyhow::Result<()>;
    }
}

#[cfg(test)]
mod tests;
