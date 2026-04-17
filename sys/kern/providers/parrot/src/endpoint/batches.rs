//! Spool backends — per-provider implementations of [`chaos_abi::SpoolBackend`].

pub mod anthropic;
pub mod xai;

pub use anthropic::AnthropicSpoolBackend;
pub use xai::XaiSpoolBackend;
