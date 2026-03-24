use crate::schema::Identity;
use crate::{Daemon, DaemonError};

impl Daemon {
    /// Set or update the daemon's identity.
    pub async fn set_identity(
        &self,
        name: &str,
        persona: Option<&str>,
    ) -> Result<Identity, DaemonError> {
        let row = sqlx::query_as::<_, Identity>(
            "INSERT INTO identity (id, name, persona)
             VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 persona = excluded.persona,
                 last_seen = unixepoch()
             RETURNING *",
        )
        .bind(name)
        .bind(persona)
        .fetch_one(self.pool())
        .await?;

        Ok(row)
    }

    /// Who am I?
    pub async fn whoami(&self) -> Result<Identity, DaemonError> {
        sqlx::query_as::<_, Identity>("SELECT * FROM identity WHERE id = 1")
            .fetch_optional(self.pool())
            .await?
            .ok_or(DaemonError::NoIdentity)
    }

    /// Touch last_seen timestamp.
    pub async fn heartbeat(&self) -> Result<(), DaemonError> {
        sqlx::query("UPDATE identity SET last_seen = unixepoch() WHERE id = 1")
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
