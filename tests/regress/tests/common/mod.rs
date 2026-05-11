//! Shared fixtures for `chaos-regress` integration tests.
//!
//! Cargo compiles each file under `tests/` as its own binary, so any
//! helper that more than one test reaches for has to be re-declared per
//! file or pulled in through the one allowed exception: `tests/common/`.
//! This module is that exception. Everything in here exists only so the
//! per-suite files can stay focused on the behavior they pin.
//!
//! Helpers genuinely should panic if the local toolchain or filesystem
//! can't cooperate — there's nothing meaningful to test if the harness
//! itself collapses, so the lint opt-out lives at the module root.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

pub mod git;
