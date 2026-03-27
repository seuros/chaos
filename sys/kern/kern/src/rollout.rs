//! Rollout module: persistence and discovery of session rollout files.

use chaos_ipc::protocol::SessionSource;

pub const SESSIONS_SUBDIR: &str = "sessions";
pub const ARCHIVED_SESSIONS_SUBDIR: &str = "archived_sessions";
pub const INTERACTIVE_SESSION_SOURCES: &[SessionSource] =
    &[SessionSource::Cli, SessionSource::VSCode];

pub(crate) mod error;
pub mod list;
pub(crate) mod metadata;
pub(crate) mod policy;
pub mod recorder;
pub(crate) mod session_index;
pub(crate) mod truncation;

pub use chaos_ipc::protocol::SessionMeta;
pub(crate) use error::map_session_init_error;
pub use list::find_archived_process_path_by_id_str;
pub use list::find_process_path_by_id_str;
pub use list::ProcessItem;
pub use list::ProcessListConfig;
pub use list::ProcessListLayout;
pub use list::ProcessSortKey;
pub use list::ProcessesPage;
pub use list::get_processes;
pub use list::get_processes_in_root;
pub use list::rollout_date_parts;
pub use recorder::RolloutRecorder;
pub use recorder::RolloutRecorderParams;
pub use session_index::append_process_name;
pub use session_index::find_process_id_by_name;
pub use session_index::find_process_name_by_id;
pub use session_index::find_process_names_by_ids;
pub use session_index::find_process_path_by_name_str;

#[cfg(test)]
pub mod tests;
