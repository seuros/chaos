//! `chaos-services` — foreign vendor service registry.
//!
//! Analog of `/etc/services`: a central directory of well-known endpoints
//! owned by third parties (model providers, auth issuers, documentation
//! portals) that chaos talks to or links the user at.
//!
//! ## Why this crate exists
//!
//! These URLs live in *other people's infrastructure*. The path segments
//! (`codex`, `v1`, `backend-api`) are product namespaces chosen by the
//! upstream vendor, not by us. A project-wide rename that rewrites
//! `codex → chaos` inside a string literal like
//! `https://chatgpt.com/backend-api/codex/responses` silently repoints the
//! client at a non-existent endpoint and every request starts returning
//! `404 {"detail":"Not Found"}`.
//!
//! Centralising these constants here makes two things true:
//!
//! 1. Bulk renames cannot touch them — the strings live in one file that
//!    sed jockeys are expected to leave alone.
//! 2. The blast radius of a vendor-side URL change is one edit, not a
//!    grep across the workspace.
//!
//! Organised by vendor module (`openai`, `anthropic`, …) so new providers
//! slot in without touching unrelated code.

#![forbid(unsafe_code)]

pub mod anthropic;
pub mod openai;

pub const THIRDPARTY_PROVIDERS_TOML: &str = include_str!("../thirdparty.toml");
