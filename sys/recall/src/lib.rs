//! Vector recall over the mounted Postgres backend, via pgvector.

pub mod pg;
pub mod store;

pub use pg::PgRecallStore;
pub use store::RecallDoc;
pub use store::RecallError;
pub use store::RecallStore;
pub use store::SearchRequest;
pub use store::SearchResult;
