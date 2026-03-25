//! The model adapter trait — the contract between the Chaos core and
//! any model provider backend.

use crate::error::AbiError;
use crate::request::TurnRequest;
use crate::stream::TurnStream;

/// A model provider adapter.
///
/// Implementations translate [`TurnRequest`] into the provider's wire
/// format, stream the response, and emit [`TurnEvent`](crate::TurnEvent)s
/// via a [`TurnStream`].
///
/// Transport setup (HTTP client, auth, WebSocket) is internal to each
/// adapter. The core does not care how the adapter talks to the provider.
pub trait ModelAdapter: Send + Sync + std::fmt::Debug {
    /// Stream a turn request to the provider and return the event stream.
    fn stream(
        &self,
        request: TurnRequest,
    ) -> impl std::future::Future<Output = Result<TurnStream, AbiError>> + Send;

    /// Provider name for telemetry and logging.
    fn provider_name(&self) -> &str;
}
