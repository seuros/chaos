use crate::schema::Skill;
use crate::{Daemon, DaemonError};

impl Daemon {
    /// Teach the daemon a new skill.
    pub async fn learn_skill(
        &self,
        name: &str,
        definition: &str,
        source: Option<&str>,
    ) -> Result<Skill, DaemonError> {
        let row = sqlx::query_as::<_, Skill>(
            "INSERT INTO skills (name, definition, source)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET
                 definition = excluded.definition,
                 source = excluded.source
             RETURNING *",
        )
        .bind(name)
        .bind(definition)
        .bind(source)
        .fetch_one(self.pool())
        .await?;

        Ok(row)
    }

    /// List all skills.
    pub async fn skills(&self) -> Result<Vec<Skill>, DaemonError> {
        let rows = sqlx::query_as::<_, Skill>("SELECT * FROM skills ORDER BY name")
            .fetch_all(self.pool())
            .await?;
        Ok(rows)
    }

    /// Forget a skill.
    pub async fn forget_skill(&self, name: &str) -> Result<(), DaemonError> {
        sqlx::query("DELETE FROM skills WHERE name = ?1")
            .bind(name)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
