#![warn(rust_2024_compatibility, clippy::all)]

//! Multi-phase memory system — extraction, consolidation, storage.
//!
//! Named after the film: persist context to survive memory loss across
//! sessions. Memories flow through three phases:
//!
//! 1. **Extraction** — distill salient facts from completed session rollouts.
//! 2. **Consolidation** — merge, deduplicate, and rank extracted memories.
//! 3. **Storage** — persist consolidated memories for injection into future sessions.

pub mod citations;
pub mod control;
pub mod prompts;
pub mod storage;
