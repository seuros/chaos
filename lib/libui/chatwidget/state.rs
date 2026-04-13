//! State mutation helpers for `ChatWidget`.
//!
//! This module collects methods that read or mutate `ChatWidget` fields without
//! belonging to rendering, protocol-event dispatch, or keyboard-event handling.
//! It covers submission flow, history management, collaboration-mode bookkeeping,
//! connector cache state, and accessors/setters for widget configuration fields.

pub(super) mod config;
pub(super) mod history;
pub(super) mod protocol_responses;
pub(super) mod submission;
