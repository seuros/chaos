//! Session history persistence, discovery, and replay over journald.

use chaos_ipc::protocol::SessionSource;

pub const INTERACTIVE_SESSION_SOURCES: &[SessionSource] =
    &[SessionSource::Cli, SessionSource::VSCode];

pub(crate) mod error;
pub mod list;
pub(crate) mod metadata;
pub(crate) mod policy;
pub mod recorder;
pub(crate) mod process_names;
pub(crate) mod truncation;

pub use chaos_ipc::protocol::SessionMeta;
pub(crate) use error::map_session_init_error;
pub use list::ProcessItem;
pub use list::ProcessSortKey;
pub use list::ProcessesPage;
pub use recorder::RolloutRecorder;
pub use recorder::RolloutRecorderParams;
pub use process_names::append_process_name;
pub use process_names::find_process_id_by_name;
pub use process_names::find_process_name_by_id;
pub use process_names::find_process_names_by_ids;
