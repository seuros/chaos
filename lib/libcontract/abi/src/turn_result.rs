//! Canonical, non-streaming counterpart of [`crate::TurnStream`].

use chaos_ipc::models::ContentItem;
use chaos_ipc::protocol::TokenUsage;
use serde::Deserialize;
use serde::Serialize;

/// One completed turn's worth of assistant output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnResult {
    Success(TurnOutput),
    Error(TurnError),
}

impl TurnResult {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    pub fn usage(&self) -> Option<&TokenUsage> {
        match self {
            Self::Success(o) => o.usage.as_ref(),
            Self::Error(e) => e.usage.as_ref(),
        }
    }

    pub fn into_output(self) -> Result<TurnOutput, TurnError> {
        match self {
            Self::Success(o) => Ok(o),
            Self::Error(e) => Err(e),
        }
    }
}

/// Assistant output for a successful turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutput {
    pub content: Vec<ContentItem>,

    /// Provider-native stop reason (`"end_turn"`, `"stop"`, `"length"`, …).
    pub finish_reason: Option<String>,

    pub usage: Option<TokenUsage>,

    /// Model the provider actually ran when it differs from the requested one.
    pub server_model: Option<String>,
}

/// Per-item provider error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnError {
    /// Provider-native error code (`"rate_limit"`, `"content_filtered"`, …).
    pub code: String,

    pub message: String,

    pub usage: Option<TokenUsage>,
}

#[cfg(test)]
mod tests;
