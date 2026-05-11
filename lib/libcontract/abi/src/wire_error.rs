//! Cross-cutting wire-format error semantics.
//!
//! Each transport / wire crate keeps its own domain-specific error enum
//! (`AbiError`, `TransportError`, `GuestError`, …). The variants differ
//! because the failure modes they surface differ. The classification of
//! a failure — *should I retry?*, *was this a timeout?*, *did the peer
//! suggest a backoff?* — is shared.
//!
//! [`WireFormatError`] is the trait that captures that shared shape so
//! retry loops, schedulers, and telemetry can reason about errors from
//! any wire layer without down-casting into a specific enum.
//!
//! The trait lives in `chaos-abi` rather than `chaos-traits` because the
//! existing `chaos-traits` crate transitively depends on `chaos-abi`
//! through the snitch → parrot edge, so re-implementing the trait on
//! `AbiError` from `chaos-traits` would be a dependency cycle.

use std::time::Duration;

/// Classification helpers shared across wire-format error enums.
///
/// Implementors keep their own variants; this trait only describes how
/// those variants project into a small set of orthogonal questions a
/// caller asks when handling a failed wire interaction.
pub trait WireFormatError: std::error::Error {
    /// `true` when the operation may succeed if attempted again.
    ///
    /// Implementors should be conservative: returning `true` invites
    /// the caller to retry, so input-validation errors, billing
    /// errors, and other terminal failures must return `false`.
    fn is_retryable(&self) -> bool;

    /// `true` when the failure was caused by a deadline elapsing
    /// before the peer responded.
    ///
    /// A timeout is usually also retryable, but the two questions are
    /// distinct: telemetry pipelines care about timeouts specifically,
    /// while retry loops only care about retryability.
    fn is_timeout(&self) -> bool;

    /// Suggested delay before retrying, when the error itself carries
    /// one (for example a `Retry-After` header propagated through a
    /// `Retryable { delay }` variant). `None` leaves the choice to the
    /// caller's backoff policy.
    fn retry_after(&self) -> Option<Duration> {
        None
    }
}
