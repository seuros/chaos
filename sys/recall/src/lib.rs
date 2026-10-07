#![doc = include_str!("../README.md")]

pub mod embedding;
pub mod memory;
pub mod pg;
pub mod service;
pub mod store;

pub use embedding::{EmbeddingProvider, LocalEmbedder};
pub use memory::{
    MemoryHandle, MemoryOptions, MemorySelection, MemorySource, MemoryStore, MemoryTimestamps,
    UseOutcome, UseReceipt,
};
pub use pg::PgRecallStore;
pub use service::{
    RecallConfig, RecallMatch, RecallResponse, RecallService, RecallState, RecallWarning,
    RetrievalMethod, SearchOptions,
};
pub use store::RecallDoc;
pub use store::RecallError;
pub use store::RecallScope;
pub use store::RecallStore;
pub use store::RetrievalFilter;
pub use store::SearchRequest;
pub use store::SearchResult;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
