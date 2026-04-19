pub mod store;

#[cfg(feature = "pgvec")]
pub mod backends;

pub use store::{RecallDoc, RecallStore, SearchRequest, SearchResult};

#[cfg(feature = "pgvec")]
pub use backends::pg::PgRecallStore;
