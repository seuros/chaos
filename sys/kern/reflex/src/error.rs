use chaos_client::TransportError;
use thiserror::Error;

use crate::judgment::JudgmentKind;

#[derive(Debug, Error)]
pub enum ReflexError {
    #[error("no reflex backend supports {0:?}")]
    Unsupported(JudgmentKind),
    #[error("policy-violation judgments require an explicit non-empty policy")]
    MissingPolicy,
    #[error("reflex backend `{backend}` failed: {message}")]
    Backend { backend: String, message: String },
    #[error("reflex backend `{backend}` returned an unreadable answer: {message}")]
    Malformed { backend: String, message: String },
}

impl ReflexError {
    /// Log-safe category without provider text.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "unsupported",
            Self::MissingPolicy => "missing_policy",
            Self::Backend { .. } => "backend",
            Self::Malformed { .. } => "malformed",
        }
    }

    pub(crate) fn transport(name: &str, error: TransportError) -> Self {
        match error {
            TransportError::Http { status, body, .. } => Self::backend(
                name,
                format!(
                    "http {}: {}",
                    status.as_u16(),
                    bounded_body(body.as_deref().unwrap_or_default())
                ),
            ),
            error => Self::backend(name, error),
        }
    }

    pub fn backend(name: &str, err: impl std::fmt::Display) -> Self {
        Self::Backend {
            backend: name.to_string(),
            message: err.to_string(),
        }
    }

    pub fn malformed(name: &str, message: impl Into<String>) -> Self {
        Self::Malformed {
            backend: name.to_string(),
            message: message.into(),
        }
    }
}

pub(crate) fn bounded_body(text: &str) -> String {
    let text = text.trim();
    text[..text.floor_char_boundary(2_048)].to_string()
}
