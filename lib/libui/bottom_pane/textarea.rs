//! The textarea owns editable composer text, placeholder elements, cursor/wrap state, and a
//! single-entry kill buffer.
//!
//! Whole-buffer replacement APIs intentionally rebuild only the visible draft state. They clear
//! element ranges and derived cursor/wrapping caches, but they keep the kill buffer intact so a
//! caller can clear or rewrite the draft and still allow `Ctrl+Y` to restore the user's most
//! recent `Ctrl+K`. This is the contract higher-level composer flows rely on after submit,
//! slash-command dispatch, and other synthetic clears.
//!
//! This module does not implement an Emacs-style multi-entry kill ring. It keeps only the most
//! recent killed span.

mod core;
mod editing;
mod elements;
mod input;
mod movement;
mod rendering;

pub use core::{TextArea, TextAreaState};

#[cfg(test)]
pub(crate) mod tests;
