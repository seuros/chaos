//! Typed model judgments. Content policies are caller-supplied.

mod error;
pub mod jev;
mod judgment;
mod local_chat;
mod minicheck;
mod router;
mod shieldgemma;

pub use crate::error::ReflexError;
pub use crate::jev::JevBackend;
pub use crate::judgment::ActionRiskSignals;
pub use crate::judgment::Judgment;
pub use crate::judgment::JudgmentKind;
pub use crate::judgment::Verdict;
pub use crate::local_chat::ChatMessage;
pub use crate::local_chat::LocalChat;
pub use crate::local_chat::YesNo;
pub use crate::minicheck::MiniCheckBackend;
pub use crate::router::Reflex;
pub use crate::router::ReflexBackend;
pub use crate::router::ReflexFuture;
pub use crate::shieldgemma::ShieldGemmaBackend;

/// Default per-request deadline, including the response body.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod tests;
