//! Journal-backed process listing types and cursor helpers.

use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use time::OffsetDateTime;
use time::PrimitiveDateTime;
use time::format_description::FormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use uuid::Uuid;

/// Returned page of process summaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessesPage {
    /// Process summaries ordered newest first.
    pub items: Vec<ProcessItem>,
    /// Opaque pagination token to resume after the last item, or `None` if end.
    pub next_cursor: Option<Cursor>,
    /// Total number of records touched while scanning this request.
    pub num_scanned_records: usize,
    /// True if a hard scan limit was hit; consider resuming with `next_cursor`.
    pub reached_scan_limit: bool,
}

/// Summary information for a process.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessItem {
    /// Process ID from persisted session metadata.
    pub process_id: Option<ProcessId>,
    /// First user message captured for this process, if any.
    pub first_user_message: Option<String>,
    /// Working directory from persisted session metadata.
    pub cwd: Option<PathBuf>,
    /// Git branch from persisted session metadata.
    pub git_branch: Option<String>,
    /// Git commit SHA from persisted session metadata.
    pub git_sha: Option<String>,
    /// Git origin URL from persisted session metadata.
    pub git_origin_url: Option<String>,
    /// Session source from persisted session metadata.
    pub source: Option<SessionSource>,
    /// Random unique nickname from session metadata for AgentControl-spawned sub-agents.
    pub agent_nickname: Option<String>,
    /// Role (agent_role) from session metadata for AgentControl-spawned sub-agents.
    pub agent_role: Option<String>,
    /// Model provider from session metadata.
    pub model_provider: Option<String>,
    /// CLI version from session metadata.
    pub cli_version: Option<String>,
    /// RFC3339 timestamp string for when the session was created, if available.
    pub created_at: Option<String>,
    /// RFC3339 timestamp string for the most recent update.
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSortKey {
    CreatedAt,
    UpdatedAt,
}

/// Pagination cursor identifying a process by timestamp and UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    ts: OffsetDateTime,
    id: Uuid,
}

impl Cursor {
    pub fn new(ts: OffsetDateTime, id: Uuid) -> Self {
        Self { ts, id }
    }

    pub fn ts(&self) -> OffsetDateTime {
        self.ts
    }

    pub fn id(&self) -> Uuid {
        self.id
    }
}

impl serde::Serialize for Cursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let ts_str = self
            .ts
            .format(&Rfc3339)
            .map_err(|e| serde::ser::Error::custom(format!("format error: {e}")))?;
        serializer.serialize_str(&format!("{ts_str}|{}", self.id))
    }
}

impl<'de> serde::Deserialize<'de> for Cursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_cursor(&s).ok_or_else(|| serde::de::Error::custom("invalid cursor"))
    }
}

/// Pagination cursor token format: "<ts>|<uuid>" where `ts` uses RFC3339 or
/// the historical `YYYY-MM-DDThh-mm-ss` UTC format.
pub fn parse_cursor(token: &str) -> Option<Cursor> {
    let (ts_str, uuid_str) = token.split_once('|')?;
    let id = Uuid::parse_str(uuid_str).ok()?;
    let ts = OffsetDateTime::parse(ts_str, &Rfc3339).ok().or_else(|| {
        let format: &[FormatItem] =
            format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]");
        PrimitiveDateTime::parse(ts_str, format)
            .ok()
            .map(PrimitiveDateTime::assume_utc)
    })?;
    Some(Cursor::new(ts, id))
}
