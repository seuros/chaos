#![warn(rust_2024_compatibility, clippy::all)]

//! Session rollout persistence — JSONL recording, thread discovery, metadata backfill.
//!
//! Every Chaos session is recorded as a JSONL rollout file. This crate owns the
//! recording lifecycle (create, append, flush), thread discovery (listing,
//! pagination, search), session naming index, and persistence policy (what gets
//! written vs filtered).

pub mod error;
pub mod list;
pub mod policy;
pub mod session_index;
pub mod truncation;

pub use list::ProcessItem;
pub use list::ProcessListConfig;
pub use list::ProcessListLayout;
pub use list::ProcessSortKey;
pub use list::ProcessesPage;
pub use list::get_processes;
pub use list::get_processes_in_root;
pub use session_index::append_process_name;
pub use session_index::find_process_id_by_name;
pub use session_index::find_process_name_by_id;
pub use session_index::find_process_names_by_ids;

pub const SESSIONS_SUBDIR: &str = "sessions";
pub const ARCHIVED_SESSIONS_SUBDIR: &str = "archived_sessions";
