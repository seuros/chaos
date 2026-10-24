//! Shared OAuth/JWT test fixtures used by kernel and PAM integration tests.
//!
//! The kernel owns the typed fixtures in its existing `test_support` module.
//! Reexport them here for downstream suites without making the kernel depend
//! on a crate that already depends on it.

#![warn(clippy::all)]

pub use chaos_kern::test_support::auth_fixtures::build_tokens;
pub use chaos_kern::test_support::auth_fixtures::make_jwt;
pub use chaos_kern::test_support::auth_fixtures::openai_auth;
pub use chaos_kern::test_support::auth_fixtures::openai_record;

#[cfg(test)]
mod tests;
