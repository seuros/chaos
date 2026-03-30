use serde::Serialize;
use sqlx::FromRow;

#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    pub ts: i64,
    pub ts_nanos: i64,
    pub level: String,
    pub target: String,
    pub message: Option<String>,
    pub process_id: Option<String>,
    pub process_uuid: Option<String>,
    pub module_path: Option<String>,
    pub file: Option<String>,
    pub line: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct LogRow {
    pub id: i64,
    pub ts: i64,
    pub ts_nanos: i64,
    pub level: String,
    pub target: String,
    pub message: Option<String>,
    pub process_id: Option<String>,
    pub process_uuid: Option<String>,
    pub file: Option<String>,
    pub line: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogTailCursor {
    pub last_id: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogTailBatch {
    pub rows: Vec<LogRow>,
    pub cursor: LogTailCursor,
}

#[derive(Clone, Debug, Default)]
pub struct LogQuery {
    pub level_upper: Option<String>,
    pub from_ts: Option<i64>,
    pub to_ts: Option<i64>,
    pub module_like: Vec<String>,
    pub file_like: Vec<String>,
    pub process_ids: Vec<String>,
    pub search: Option<String>,
    pub include_processless: bool,
    /// When set, scope results to a single process and optionally include
    /// processless companion logs from that process's latest process UUID.
    pub related_to_process_id: Option<String>,
    /// Only meaningful when `related_to_process_id` is set.
    pub include_related_processless: bool,
    pub after_id: Option<i64>,
    pub limit: Option<usize>,
    pub descending: bool,
}
