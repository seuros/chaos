use std::future::Future;
use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{RecallDoc, RecallError, RecallScope, RecallStore};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemorySource {
    File {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fragment: Option<String>,
    },
}

impl MemorySource {
    pub fn validate(&self) -> Result<(), RecallError> {
        let Self::File { path, fragment } = self;
        if !path.is_absolute()
            || path.as_os_str().len() > 2000
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            || path.as_os_str().as_encoded_bytes().contains(&0)
            || fragment.as_ref().is_some_and(|value| value.len() > 200)
        {
            return Err(RecallError::InvalidInput("invalid file source"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryOptions {
    pub sources: Vec<MemorySource>,
    pub automatic: bool,
    pub charge: f64,
}

impl Default for MemoryOptions {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            automatic: false,
            charge: 0.5,
        }
    }
}

impl MemoryOptions {
    pub fn validate(&self) -> Result<(), RecallError> {
        if self.sources.len() > 20
            || !self.charge.is_finite()
            || !(0.0..=1.0).contains(&self.charge)
        {
            return Err(RecallError::InvalidInput("invalid memory options"));
        }
        self.sources.iter().try_for_each(MemorySource::validate)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryHandle {
    pub revision: String,
    #[serde(flatten)]
    pub timestamps: MemoryTimestamps,
    #[serde(flatten)]
    pub options: MemoryOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTimestamps {
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
}

impl Default for MemoryTimestamps {
    fn default() -> Self {
        Self {
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySelection {
    pub scope: RecallScope,
    pub id: String,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UseReceipt {
    pub id: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UseOutcome {
    pub charge: f64,
    pub applied: bool,
}

pub trait MemoryStore: RecallStore {
    fn index_memory(
        &self,
        doc: RecallDoc,
        options: MemoryOptions,
    ) -> impl Future<Output = Result<(), RecallError>> + Send;
    fn issue_receipt(
        &self,
        owner: &str,
        generation: &str,
        selections: &[MemorySelection],
    ) -> impl Future<Output = Result<UseReceipt, RecallError>> + Send;
    fn recalled_memory(
        &self,
        owner: &str,
        generation: &str,
        receipt: &str,
        scope: &RecallScope,
        id: &str,
    ) -> impl Future<Output = Result<MemoryHandle, RecallError>> + Send;
    fn commit_use(
        &self,
        owner: &str,
        generation: &str,
        receipt: &str,
        scope: &RecallScope,
        id: &str,
    ) -> impl Future<Output = Result<UseOutcome, RecallError>> + Send;
}
