use std::future::Future;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const EMBEDDING_DIM: usize = 256;

#[derive(Debug, Error)]
pub enum RecallError {
    #[error("backend error: {0}")]
    Backend(#[from] anyhow::Error),
    #[error("dimension mismatch: expected {expected}, got {got}")]
    DimMismatch { expected: usize, got: usize },
    #[error("invalid recall input: {0}")]
    InvalidInput(&'static str),
    #[error("local recall model could not be loaded: {0}")]
    ModelLoad(#[source] anyhow::Error),
    #[error(
        "recall model differs from the indexed model; reindex explicitly before changing models"
    )]
    ModelMismatch,
    #[error("embedding must be finite and nonzero")]
    InvalidEmbedding,
    #[error("recall request cancelled")]
    Cancelled,
    #[error("recall search exceeded its deadline")]
    Timeout,
    #[error("both retrieval branches failed (semantic: {semantic}; lexical: {lexical})")]
    RetrievalFailed {
        semantic: Box<Self>,
        lexical: Box<Self>,
    },
    #[error("recall workflow failed: {0}")]
    Workflow(String),
    #[error("recall receipt is invalid, expired, or belongs to changed memory")]
    InvalidReceipt,
}

/// Explicit namespace supplied by trusted callers. Legacy rows have no public scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum RecallScope {
    Global,
    Project(String),
    Session(String),
}

impl RecallScope {
    pub fn validate(&self) -> Result<(), RecallError> {
        match self {
            Self::Project(id) | Self::Session(id) if id.trim().is_empty() => {
                Err(RecallError::InvalidInput("scope ID is empty"))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn parts(&self) -> (&'static str, &str) {
        match self {
            Self::Global => ("global", ""),
            Self::Project(id) => ("project", id),
            Self::Session(id) => ("session", id),
        }
    }

    pub(crate) fn from_parts(kind: String, id: String) -> Result<Self, RecallError> {
        match kind.as_str() {
            "global" if id.is_empty() => Ok(Self::Global),
            "project" => Ok(Self::Project(id)),
            "session" => Ok(Self::Session(id)),
            _ => Err(RecallError::InvalidInput("invalid stored scope")),
        }
    }
}

/// A document with a precomputed embedding, used by storage adapters.
#[derive(Debug, Clone)]
pub struct RecallDoc {
    pub scope: RecallScope,
    pub id: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub model_fingerprint: String,
    /// Source/provenance metadata; preserved verbatim in results.
    pub metadata: serde_json::Value,
}

impl RecallDoc {
    pub(crate) fn validate(&self) -> Result<(), RecallError> {
        validate_document(&self.scope, &self.id, &self.content)?;
        if self.model_fingerprint.is_empty() {
            return Err(RecallError::InvalidInput("model fingerprint is empty"));
        }
        validate_embedding(&self.embedding)
    }
}

/// Shared preflight for text ingestion and already-embedded storage documents.
pub(crate) fn validate_document(
    scope: &RecallScope,
    id: &str,
    content: &str,
) -> Result<(), RecallError> {
    scope.validate()?;
    if id.trim().is_empty() || content.trim().is_empty() {
        return Err(RecallError::InvalidInput("document ID or content is empty"));
    }
    Ok(())
}

/// Scope is applied before ranking and limiting, by both retrieval branches.
#[derive(Debug, Clone)]
pub struct RetrievalFilter {
    pub scope: RecallScope,
    pub include_global: bool,
    pub limit: usize,
    pub model_fingerprint: String,
    pub automatic_only: bool,
}

impl RetrievalFilter {
    pub(crate) fn validate(&self) -> Result<(), RecallError> {
        self.scope.validate()?;
        if self.limit == 0 || self.limit > 250 || self.model_fingerprint.is_empty() {
            return Err(RecallError::InvalidInput(
                "invalid retrieval limit or model",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub filter: RetrievalFilter,
    pub query_vec: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub scope: RecallScope,
    pub id: String,
    /// Branch-specific similarity/rank, not a fused score.
    pub score: f32,
    pub content: String,
    pub metadata: serde_json::Value,
    pub handle: crate::MemoryHandle,
}

/// Storage abstraction; all operations require an explicit namespace.
pub trait RecallStore: Send + Sync + 'static {
    fn index(&self, doc: RecallDoc) -> impl Future<Output = Result<(), RecallError>> + Send;
    fn index_batch(
        &self,
        docs: Vec<RecallDoc>,
    ) -> impl Future<Output = Result<(), RecallError>> + Send;
    fn search(
        &self,
        req: &SearchRequest,
    ) -> impl Future<Output = Result<Vec<SearchResult>, RecallError>> + Send;
    fn lexical_search(
        &self,
        query: &str,
        filter: &RetrievalFilter,
    ) -> impl Future<Output = Result<Vec<SearchResult>, RecallError>> + Send;
    fn delete(
        &self,
        scope: &RecallScope,
        id: &str,
    ) -> impl Future<Output = Result<(), RecallError>> + Send;
}

pub fn validate_embedding(v: &[f32]) -> Result<(), RecallError> {
    if v.len() != EMBEDDING_DIM {
        return Err(RecallError::DimMismatch {
            expected: EMBEDDING_DIM,
            got: v.len(),
        });
    }
    if v.iter().any(|value| !value.is_finite()) || v.iter().all(|value| *value == 0.0) {
        return Err(RecallError::InvalidEmbedding);
    }
    Ok(())
}

pub fn normalize_embedding(mut v: Vec<f32>) -> Result<Vec<f32>, RecallError> {
    validate_embedding(&v)?;
    let norm = v
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    for value in &mut v {
        *value = (f64::from(*value) / norm) as f32;
    }
    validate_embedding(&v)?;
    Ok(v)
}
