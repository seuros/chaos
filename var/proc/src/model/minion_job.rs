use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinionJobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl MinionJobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            MinionJobStatus::Pending => "pending",
            MinionJobStatus::Running => "running",
            MinionJobStatus::Completed => "completed",
            MinionJobStatus::Failed => "failed",
            MinionJobStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(anyhow::anyhow!("invalid minion job status: {value}")),
        }
    }

    pub fn is_final(self) -> bool {
        matches!(
            self,
            MinionJobStatus::Completed | MinionJobStatus::Failed | MinionJobStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinionJobItemStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl MinionJobItemStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            MinionJobItemStatus::Pending => "pending",
            MinionJobItemStatus::Running => "running",
            MinionJobItemStatus::Completed => "completed",
            MinionJobItemStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(anyhow::anyhow!("invalid minion job item status: {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MinionJob {
    pub id: String,
    pub name: String,
    pub status: MinionJobStatus,
    pub instruction: String,
    pub auto_export: bool,
    pub max_runtime_seconds: Option<u64>,
    // TODO(jif-oai): Convert to JSON Schema and enforce structured outputs.
    pub output_schema_json: Option<Value>,
    pub input_headers: Vec<String>,
    pub input_csv_path: String,
    pub output_csv_path: String,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
    pub started_at: Option<jiff::Timestamp>,
    pub completed_at: Option<jiff::Timestamp>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MinionJobItem {
    pub job_id: String,
    pub item_id: String,
    pub row_index: i64,
    pub source_id: Option<String>,
    pub row_json: Value,
    pub status: MinionJobItemStatus,
    pub assigned_process_id: Option<String>,
    pub attempt_count: i64,
    pub result_json: Option<Value>,
    pub last_error: Option<String>,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
    pub completed_at: Option<jiff::Timestamp>,
    pub reported_at: Option<jiff::Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinionJobProgress {
    pub total_items: usize,
    pub pending_items: usize,
    pub running_items: usize,
    pub completed_items: usize,
    pub failed_items: usize,
}

#[derive(Debug, Clone)]
pub struct MinionJobCreateParams {
    pub id: String,
    pub name: String,
    pub instruction: String,
    pub auto_export: bool,
    pub max_runtime_seconds: Option<u64>,
    pub output_schema_json: Option<Value>,
    pub input_headers: Vec<String>,
    pub input_csv_path: String,
    pub output_csv_path: String,
}

#[derive(Debug, Clone)]
pub struct MinionJobItemCreateParams {
    pub item_id: String,
    pub row_index: i64,
    pub source_id: Option<String>,
    pub row_json: Value,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct MinionJobRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) instruction: String,
    pub(crate) auto_export: i64,
    pub(crate) max_runtime_seconds: Option<i64>,
    pub(crate) output_schema_json: Option<String>,
    pub(crate) input_headers_json: String,
    pub(crate) input_csv_path: String,
    pub(crate) output_csv_path: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) completed_at: Option<i64>,
    pub(crate) last_error: Option<String>,
}

impl TryFrom<MinionJobRow> for MinionJob {
    type Error = anyhow::Error;

    fn try_from(value: MinionJobRow) -> Result<Self, Self::Error> {
        let output_schema_json = value
            .output_schema_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        let input_headers = serde_json::from_str(value.input_headers_json.as_str())?;
        let max_runtime_seconds = value
            .max_runtime_seconds
            .map(u64::try_from)
            .transpose()
            .map_err(|_| anyhow::anyhow!("invalid max_runtime_seconds value"))?;
        Ok(Self {
            id: value.id,
            name: value.name,
            status: MinionJobStatus::parse(value.status.as_str())?,
            instruction: value.instruction,
            auto_export: value.auto_export != 0,
            max_runtime_seconds,
            output_schema_json,
            input_headers,
            input_csv_path: value.input_csv_path,
            output_csv_path: value.output_csv_path,
            created_at: epoch_seconds_to_datetime(value.created_at)?,
            updated_at: epoch_seconds_to_datetime(value.updated_at)?,
            started_at: value
                .started_at
                .map(epoch_seconds_to_datetime)
                .transpose()?,
            completed_at: value
                .completed_at
                .map(epoch_seconds_to_datetime)
                .transpose()?,
            last_error: value.last_error,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct MinionJobItemRow {
    pub(crate) job_id: String,
    pub(crate) item_id: String,
    pub(crate) row_index: i64,
    pub(crate) source_id: Option<String>,
    pub(crate) row_json: String,
    pub(crate) status: String,
    pub(crate) assigned_process_id: Option<String>,
    pub(crate) attempt_count: i64,
    pub(crate) result_json: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) completed_at: Option<i64>,
    pub(crate) reported_at: Option<i64>,
}

impl TryFrom<MinionJobItemRow> for MinionJobItem {
    type Error = anyhow::Error;

    fn try_from(value: MinionJobItemRow) -> Result<Self, Self::Error> {
        Ok(Self {
            job_id: value.job_id,
            item_id: value.item_id,
            row_index: value.row_index,
            source_id: value.source_id,
            row_json: serde_json::from_str(value.row_json.as_str())?,
            status: MinionJobItemStatus::parse(value.status.as_str())?,
            assigned_process_id: value.assigned_process_id,
            attempt_count: value.attempt_count,
            result_json: value
                .result_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            last_error: value.last_error,
            created_at: epoch_seconds_to_datetime(value.created_at)?,
            updated_at: epoch_seconds_to_datetime(value.updated_at)?,
            completed_at: value
                .completed_at
                .map(epoch_seconds_to_datetime)
                .transpose()?,
            reported_at: value
                .reported_at
                .map(epoch_seconds_to_datetime)
                .transpose()?,
        })
    }
}

fn epoch_seconds_to_datetime(secs: i64) -> Result<jiff::Timestamp> {
    jiff::Timestamp::from_second(secs)
        .map_err(|err| anyhow::anyhow!("invalid unix timestamp {secs}: {err}"))
}
