//! Context-window management for chaos: how history is distilled when the
//! window fills, how token allotments are rationed, and how conversation
//! items are classified.
//!
//! The kernel owns session orchestration; this crate owns the pure logic.

pub mod allotment;
pub mod contextual_user_message;
pub mod distill;
pub mod event_mapping;
pub mod pressure;
pub mod web_search;
