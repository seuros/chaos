#![warn(rust_2024_compatibility, clippy::all)]

//! Session rollout persistence — JSONL recording, thread discovery, metadata backfill.
//!
//! Every Chaos session is recorded as a JSONL rollout file. This crate owns the
//! recording lifecycle (create, append, flush), thread discovery (listing,
//! pagination, search), session naming index, and persistence policy (what gets
//! written vs filtered).

pub mod error;
pub mod list;
pub mod policy;
pub mod session_index;
pub mod truncation;

pub const SESSIONS_SUBDIR: &str = "sessions";
pub const ARCHIVED_SESSIONS_SUBDIR: &str = "archived_sessions";
