//! UsageStore — persistence for rate-limit snapshots sniffed off the wire.
//!
//! The `HeaderExtractor` in `chaos-ration` turns raw response headers into
//! [`UsageWindow`] values; this crate takes it from there and puts them in
//! the runtime database. The latest reading per (provider, label) is kept
//! for instant "85% left" answers; every snapshot is also appended to an
//! append-only history table so trends survive restarts forever.
//!
//! Reset semantics matter: rate-limit headers only flow on live responses,
//! so between requests the last-seen value ages. Past `resets_at`, the
//! budget has already refilled and the stored numbers are actively
//! misleading. Reads surface a [`Freshness`] so callers can decide whether
//! to trust the snapshot or fall back to "budget recovered".

pub mod middleware;
pub mod store;

pub use middleware::RationLayer;
pub use middleware::RationService;
pub use store::LatestWindow;
pub use store::UsageStore;

pub use chaos_ration::Freshness;
pub use chaos_ration::UsageWindow;
