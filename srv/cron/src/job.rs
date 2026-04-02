//! Cron job model types.

use serde::Deserialize;
use serde::Serialize;

/// Scope determines the lifetime and visibility of a cron job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronScope {
    /// Persists across sessions, tied to a project directory.
    Project,
    /// Lives only for the current session.
    Session,
    /// Self-scheduled by an agent, ephemeral unless promoted.
    Agent,
}

impl CronScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Session => "session",
            Self::Agent => "agent",
        }
    }
}

impl std::fmt::Display for CronScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CronScope {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "project" => Ok(Self::Project),
            "session" => Ok(Self::Session),
            "agent" => Ok(Self::Agent),
            other => anyhow::bail!("unknown cron scope: {other}"),
        }
    }
}

/// A scheduled job stored in chaos.sqlite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    /// Cron expression (e.g., "*/5 * * * *") or interval shorthand (e.g., "5m").
    pub schedule: String,
    /// The command or action to execute.
    pub command: String,
    pub scope: CronScope,
    /// For project-scoped jobs, the directory they belong to.
    pub project_path: Option<String>,
    /// For session/agent-scoped jobs, the owning session ID.
    pub session_id: Option<String>,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Parameters for creating a new cron job.
#[derive(Debug, Clone)]
pub struct CreateJobParams {
    pub name: String,
    pub schedule: String,
    pub command: String,
    pub scope: CronScope,
    pub project_path: Option<String>,
    pub session_id: Option<String>,
}
