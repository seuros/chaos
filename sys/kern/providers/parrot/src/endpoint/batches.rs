//! Spool backends — per-provider implementations of [`chaos_abi::SpoolBackend`].

use chaos_abi::SpoolError;
use rama::http::StatusCode;

pub mod anthropic;
pub mod xai;

pub use anthropic::AnthropicSpoolBackend;
pub use xai::XaiSpoolBackend;

/// Maps a non-success HTTP status from a spool provider onto a [`SpoolError`].
/// Shared by every provider backend, which classify batch responses identically.
fn classify_status(status: StatusCode, body: &str) -> Result<(), SpoolError> {
    if status.is_success() {
        return Ok(());
    }
    let code = status.as_u16();
    Err(match code {
        401 | 403 => SpoolError::Auth,
        429 => SpoolError::RateLimit { retry_after: None },
        _ => SpoolError::ProviderError {
            status: code,
            message: body.to_string(),
        },
    })
}
