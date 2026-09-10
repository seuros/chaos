//! Owner-scoped settings and installation-scoped grants.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SettingsSnapshot {
    pub revision: i64,
    pub settings: Value,
    pub migration_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Active,
    Revoked,
    PendingReapproval,
}

impl ApprovalState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::PendingReapproval => "pending_reapproval",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RememberedApproval {
    pub id: String,
    pub installation_id: String,
    pub scope: String,
    pub kind: String,
    pub subject: String,
    pub identity: String,
    pub state: ApprovalState,
    pub payload: Value,
}

fn now() -> anyhow::Result<i64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}

impl PostgresRuntime {
    fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// Numbered placeholders let both backends share authorization transactions.
macro_rules! settings_backend {
    ($runtime:ty) => {
        impl $runtime {
            async fn settings_snapshot(&self) -> anyhow::Result<SettingsSnapshot> {
                let row = sqlx::query(
                    "SELECT schema_version, revision, settings_json, migration_digest FROM user_settings WHERE id = 1",
                )
                .fetch_one(self.pool())
                .await?;
                anyhow::ensure!(row.try_get::<i64, _>("schema_version")? == 1, "unsupported settings schema");
                Ok(SettingsSnapshot {
                    revision: row.try_get("revision")?,
                    settings: serde_json::from_str(&row.try_get::<String, _>("settings_json")?)?,
                    migration_digest: row.try_get("migration_digest")?,
                })
            }

            async fn commit_settings(
                &self,
                expected_revision: i64,
                settings: &Value,
                migration_digest: Option<&str>,
            ) -> anyhow::Result<i64> {
                self.commit_settings_import(expected_revision, settings, migration_digest, &[], None).await
            }

            async fn commit_settings_import(
                &self,
                expected_revision: i64,
                settings: &Value,
                migration_digest: Option<&str>,
                pending_grants: &[RememberedApproval],
                policy_sources: Option<&Value>,
            ) -> anyhow::Result<i64> {
                anyhow::ensure!(settings.is_object(), "settings must be an object");
                anyhow::ensure!(pending_grants.iter().all(|grant| grant.state == ApprovalState::PendingReapproval),
                    "import cannot activate grants");
                let mut tx = self.pool().begin().await?;
                let result = sqlx::query(
                    "UPDATE user_settings SET settings_json = $1, revision = revision + 1, migration_digest = COALESCE($2, migration_digest) WHERE id = 1 AND revision = $3 AND schema_version = 1",
                )
                .bind(serde_json::to_string(settings)?)
                .bind(migration_digest)
                .bind(expected_revision)
                .execute(&mut *tx)
                .await?;
                anyhow::ensure!(result.rows_affected() == 1, "settings revision conflict; reload and retry");
                if let Some(sources) = policy_sources {
                    anyhow::ensure!(sources.is_array(), "policy sources must be an array");
                    sqlx::query("UPDATE user_policy_sources SET sources_json = $1 WHERE id = 1")
                        .bind(serde_json::to_string(sources)?).execute(&mut *tx).await?;
                }
                for grant in pending_grants {
                    sqlx::query(
                        "INSERT INTO remembered_approvals (id, installation_id, scope, kind, subject, identity, state, payload_json, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,'pending_reapproval',$7,$8,$8) ON CONFLICT (installation_id, scope, kind, subject, identity) DO NOTHING",
                    )
                    .bind(&grant.id).bind(&grant.installation_id).bind(&grant.scope)
                    .bind(&grant.kind).bind(&grant.subject).bind(&grant.identity)
                    .bind(serde_json::to_string(&grant.payload)?).bind(now()?)
                    .execute(&mut *tx).await?;
                }
                sqlx::query("INSERT INTO configuration_events (id, event, subject, created_at) VALUES ($1, $2, $3, $4)")
                    .bind(Uuid::now_v7().to_string())
                    .bind(if migration_digest.is_some() { "settings_migrated" } else { "settings_updated" })
                    .bind("user")
                    .bind(now()?)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok(expected_revision + 1)
            }

            async fn approval_revision(&self) -> anyhow::Result<i64> {
                Ok(sqlx::query("SELECT revision FROM approval_revision WHERE id = 1")
                    .fetch_one(self.pool()).await?.try_get("revision")?)
            }

            async fn user_policy_sources(&self) -> anyhow::Result<Value> {
                let row = sqlx::query("SELECT sources_json FROM user_policy_sources WHERE id = 1")
                    .fetch_one(self.pool()).await?;
                Ok(serde_json::from_str(&row.try_get::<String, _>("sources_json")?)?)
            }

            async fn list_approvals(&self, installation_id: &str) -> anyhow::Result<Vec<RememberedApproval>> {
                let rows = sqlx::query(
                    "SELECT id, installation_id, scope, kind, subject, identity, state, payload_json FROM remembered_approvals WHERE installation_id = $1 ORDER BY id",
                ).bind(installation_id).fetch_all(self.pool()).await?;
                rows.into_iter().map(|row| {
                    let state: String = row.try_get("state")?;
                    Ok(RememberedApproval {
                        id: row.try_get("id")?,
                        installation_id: row.try_get("installation_id")?,
                        scope: row.try_get("scope")?,
                        kind: row.try_get("kind")?,
                        subject: row.try_get("subject")?,
                        identity: row.try_get("identity")?,
                        state: match state.as_str() {
                            "active" => ApprovalState::Active,
                            "revoked" => ApprovalState::Revoked,
                            "pending_reapproval" => ApprovalState::PendingReapproval,
                            _ => anyhow::bail!("invalid approval state"),
                        },
                        payload: serde_json::from_str(&row.try_get::<String, _>("payload_json")?)?,
                    })
                }).collect()
            }

            async fn put_approval(&self, approval: &RememberedApproval) -> anyhow::Result<()> {
                let mut tx = self.pool().begin().await?;
                let timestamp = now()?;
                let persisted_id: String = sqlx::query(
                    "INSERT INTO remembered_approvals (id, installation_id, scope, kind, subject, identity, state, payload_json, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) ON CONFLICT (installation_id, scope, kind, subject, identity) DO UPDATE SET state = excluded.state, payload_json = excluded.payload_json, updated_at = excluded.updated_at RETURNING id",
                )
                .bind(&approval.id).bind(&approval.installation_id).bind(&approval.scope)
                .bind(&approval.kind).bind(&approval.subject).bind(&approval.identity)
                .bind(approval.state.as_str()).bind(serde_json::to_string(&approval.payload)?)
                .bind(timestamp).fetch_one(&mut *tx).await?.try_get("id")?;
                sqlx::query("UPDATE approval_revision SET revision = revision + 1 WHERE id = 1")
                    .execute(&mut *tx).await?;
                sqlx::query("INSERT INTO configuration_events (id, event, subject, created_at) VALUES ($1,$2,$3,$4)")
                    .bind(Uuid::now_v7().to_string()).bind(approval.state.as_str())
                    .bind(&persisted_id).bind(timestamp).execute(&mut *tx).await?;
                tx.commit().await?;
                Ok(())
            }

            async fn revoke_approvals(&self, installation_id: &str, id: Option<&str>) -> anyhow::Result<u64> {
                let mut tx = self.pool().begin().await?;
                // Advance even for absent grants to invalidate session approvals.
                let result = sqlx::query(
                    "UPDATE remembered_approvals SET state = 'revoked', updated_at = $1 WHERE installation_id = $2 AND ($3 IS NULL OR id = $3)",
                ).bind(now()?).bind(installation_id).bind(id).execute(&mut *tx).await?;
                sqlx::query("UPDATE approval_revision SET revision = revision + 1 WHERE id = 1")
                    .execute(&mut *tx).await?;
                sqlx::query("INSERT INTO configuration_events (id, event, subject, created_at) VALUES ($1,$2,$3,$4)")
                    .bind(Uuid::now_v7().to_string()).bind("approvals_revoked")
                    .bind(id.unwrap_or("all")).bind(now()?).execute(&mut *tx).await?;
                tx.commit().await?;
                Ok(result.rows_affected())
            }
        }
    };
}

settings_backend!(StateRuntime);
settings_backend!(PostgresRuntime);

impl RuntimeDbHandle {
    chaos_dispatch::backend_dispatch! {
        pub async fn settings_snapshot(&self) -> anyhow::Result<SettingsSnapshot>;
        pub async fn commit_settings(&self, expected_revision: i64, settings: &Value, migration_digest: Option<&str>) -> anyhow::Result<i64>;
        pub async fn commit_settings_import(&self, expected_revision: i64, settings: &Value, migration_digest: Option<&str>, pending_grants: &[RememberedApproval], policy_sources: Option<&Value>) -> anyhow::Result<i64>;
        pub async fn user_policy_sources(&self) -> anyhow::Result<Value>;
        pub async fn approval_revision(&self) -> anyhow::Result<i64>;
        pub async fn list_approvals(&self, installation_id: &str) -> anyhow::Result<Vec<RememberedApproval>>;
        pub async fn put_approval(&self, approval: &RememberedApproval) -> anyhow::Result<()>;
        pub async fn revoke_approvals(&self, installation_id: &str, id: Option<&str>) -> anyhow::Result<u64>;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn settings_cas_and_installation_scoped_revocation() -> anyhow::Result<()> {
        let home = std::env::temp_dir().join(format!("chaos-settings-{}", Uuid::now_v7()));
        let runtime =
            RuntimeDbHandle::Sqlite(StateRuntime::init(home.clone(), "test".into()).await?);
        let snapshot = runtime.settings_snapshot().await?;
        assert_eq!(snapshot.revision, 0);
        runtime
            .commit_settings(0, &serde_json::json!({"model":"test"}), Some("digest"))
            .await?;
        assert!(
            runtime
                .commit_settings(0, &serde_json::json!({}), None)
                .await
                .is_err()
        );
        assert_eq!(runtime.settings_snapshot().await?.settings["model"], "test");
        let mut approval = RememberedApproval {
            id: Uuid::now_v7().to_string(),
            installation_id: "one".into(),
            scope: "/project".into(),
            kind: "mcp".into(),
            subject: "server/tool".into(),
            identity: "v1:identity".into(),
            state: ApprovalState::Active,
            payload: Value::Null,
        };
        runtime.put_approval(&approval).await?;
        approval.id = Uuid::now_v7().to_string();
        approval.installation_id = "two".into();
        runtime.put_approval(&approval).await?;
        let revision = runtime.approval_revision().await?;
        assert_eq!(runtime.revoke_approvals("one", None).await?, 1);
        assert!(runtime.approval_revision().await? > revision);
        assert_eq!(
            runtime.list_approvals("one").await?[0].state,
            ApprovalState::Revoked
        );
        assert_eq!(
            runtime.list_approvals("two").await?[0].state,
            ApprovalState::Active
        );
        drop(runtime);
        std::fs::remove_dir_all(home)?;
        Ok(())
    }
}
