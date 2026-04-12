//! Status output formatting and display adapters for the TUI.
//!
//! This module turns protocol-level snapshots into stable display structures used by `/status`
//! output and footer/status-line helpers, while keeping rendering concerns out of transport-facing
//! code.
//!
//! `rate_limits` is the main integration point for status-line usage-limit items: it converts raw
//! window snapshots into local-time labels and classifies data as available, stale, or missing.
mod account;
mod card;
mod format;
mod helpers;
mod rate_limits;

#[cfg(test)]
pub use card::new_status_output;
pub use card::new_status_output_with_rate_limits;
pub use helpers::format_directory_display;
pub use helpers::format_tokens_compact;
pub use rate_limits::RateLimitSnapshotDisplay;
pub use rate_limits::RateLimitWindowDisplay;
#[cfg(test)]
pub use rate_limits::rate_limit_snapshot_display;
pub use rate_limits::rate_limit_snapshot_display_for_limit;

#[cfg(test)]
mod tests;
