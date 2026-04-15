//! chaos-regress — one test suite to rule them all.
//!
//! The universe trends toward disorder. This crate is where we push
//! back: behavioral tests for chaos as a whole system, living in one
//! tree at the workspace root so the entropy stays here instead of
//! seeping into every `src/` file.
//!
//! Inline `#[cfg(test)]` in member crates is reserved for tests that
//! need access to private items or accelerate debugging at module
//! boundaries. Everything else belongs in `tests/regress/tests/`.
