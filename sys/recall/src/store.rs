use std::future::Future;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecallError {
    #[error("backend error: {0}")]
    Backend(#[from] anyhow::Error),
    #[error("dimension mismatch: expected {expected}, got {got}")]
    DimMismatch { expected: usize, got: usize },
}

/// A document to index.
#[derive(Debug, Clone)]
pub struct RecallDoc {
    /// Stable identifier (e.g. file path, note id).
    pub id: String,
    /// Plain-text content to surface in search results.
    pub content: String,
    /// Pre-computed embedding vector.
    pub embedding: Vec<f32>,
    /// Arbitrary metadata stored alongside the document.
    pub metadata: serde_json::Value,
}

/// Parameters for a nearest-neighbour search.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Pre-computed query embedding.
    pub query_vec: Vec<f32>,
    /// Maximum results to return.
    pub limit: usize,
    /// Discard results below this cosine similarity (0..1).
    pub min_score: Option<f32>,
    /// HNSW ef_search override (pgvector: `hnsw.ef_search`).
    pub ef_search: Option<i32>,
}

impl SearchRequest {
    pub fn new(query_vec: Vec<f32>, limit: usize) -> Self {
        Self {
            query_vec,
            limit,
            min_score: None,
            ef_search: None,
        }
    }

    pub fn with_min_score(mut self, score: f32) -> Self {
        self.min_score = Some(score);
        self
    }

    pub fn with_ef_search(mut self, ef: i32) -> Self {
        self.ef_search = Some(ef);
        self
    }
}

/// A single search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub content: String,
    pub metadata: serde_json::Value,
}

/// Async vector store abstraction.
///
/// Implementations: [`PgRecallStore`] (pgvector), sqlite-vec (future).
pub trait RecallStore: Send + Sync {
    fn index(&self, doc: RecallDoc) -> impl Future<Output = Result<(), RecallError>> + Send;
    fn index_batch(
        &self,
        docs: Vec<RecallDoc>,
    ) -> impl Future<Output = Result<(), RecallError>> + Send;
    fn search(
        &self,
        req: &SearchRequest,
    ) -> impl Future<Output = Result<Vec<SearchResult>, RecallError>> + Send;
    fn delete(&self, id: &str) -> impl Future<Output = Result<(), RecallError>> + Send;
}
