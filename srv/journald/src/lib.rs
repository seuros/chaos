#![warn(rust_2024_compatibility, clippy::all)]

//! Session journal service — canonical append-only process history with SQLite default.
//!
//! This crate is the starting point for moving Chaos session persistence from
//! file-primary rollout JSONL into a backend-neutral journal service:
//! - the journal is append-only and replayable
//! - per-process writer ownership is enforced with leases
//! - SQLite is the default backend today
//! - the API is shaped so Postgres can be added later without changing callers

mod bootstrap;
mod client;
mod error;
mod model;
mod protocol;
mod rama_http;
mod server;
mod sqlite;
mod store;

pub use bootstrap::BootstrapPaths;
pub use bootstrap::DEFAULT_BOOTSTRAP_TIMEOUT;
pub use bootstrap::ensure_sqlite_journald_running;
pub use bootstrap::runtime_socket_dir;
pub use client::JournalClientError;
pub use client::JournalRpcClient;
pub use error::JournalError;
pub use model::AppendBatchInput;
pub use model::AppendBatchResult;
pub use model::CreateProcessInput;
pub use model::EntrySeq;
pub use model::HelloRequest;
pub use model::HelloResponse;
pub use model::JournalEntry;
pub use model::Lease;
pub use model::LeaseToken;
pub use model::LoadedJournal;
pub use model::OwnerId;
pub use model::ParentRef;
pub use model::ProcessRecord;
pub use protocol::AcquireLeaseRequest;
pub use protocol::CreateProcessResponse;
pub use protocol::ErrorCode;
pub use protocol::ErrorPayload;
pub use protocol::GetProcessRequest;
pub use protocol::GetProcessResponse;
pub use protocol::HeartbeatLeaseRequest;
pub use protocol::JournalRequest;
pub use protocol::JournalResponse;
pub use protocol::ListProcessesRequest;
pub use protocol::ListProcessesResponse;
pub use protocol::LoadJournalRequest;
pub use protocol::ReleaseLeaseRequest;
pub use protocol::ReleaseLeaseResponse;
pub use protocol::RequestEnvelope;
pub use protocol::ResponseEnvelope;
pub use rama_http::JOURNAL_RPC_PATH;
pub use rama_http::JournalRpcServer;
pub use server::JournalServerConfig;
pub use server::default_socket_path;
pub use server::default_socket_runtime_dir;
pub use server::run_sqlite_journal_server;
pub use server::sqlite_db_path;
pub use sqlite::SqliteJournalStore;
pub use store::JournalStore;
