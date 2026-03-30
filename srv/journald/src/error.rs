#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),

    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialize {field}: {source}")]
    Serialize {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("deserialize {field}: {source}")]
    Deserialize {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid process id `{value}`: {source}")]
    InvalidProcessId {
        value: String,
        #[source]
        source: uuid::Error,
    },

    #[error("invalid timestamp `{value}`: {message}")]
    InvalidTimestamp { value: String, message: String },

    #[error("process already exists: {0}")]
    ProcessAlreadyExists(chaos_ipc::ProcessId),

    #[error("process not found: {0}")]
    ProcessNotFound(chaos_ipc::ProcessId),

    #[error("lease conflict for process {process_id}")]
    LeaseConflict {
        process_id: chaos_ipc::ProcessId,
        current_owner_id: String,
        expires_at: jiff::Timestamp,
    },

    #[error("lease expired for process {process_id}")]
    LeaseExpired { process_id: chaos_ipc::ProcessId },

    #[error("invalid lease for process {process_id}")]
    InvalidLease { process_id: chaos_ipc::ProcessId },

    #[error(
        "sequence conflict for process {process_id}: expected next seq {expected_next_seq}, actual {actual_next_seq}"
    )]
    SequenceConflict {
        process_id: chaos_ipc::ProcessId,
        expected_next_seq: i64,
        actual_next_seq: i64,
    },
}
