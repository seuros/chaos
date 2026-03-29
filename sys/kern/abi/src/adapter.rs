//! The model adapter trait — the contract between the Chaos core and
//! any model provider backend.
//!
//! The trait is **object-safe**: `stream()` returns a boxed future so
//! the kernel can hold `Box<dyn ModelAdapter>` and dispatch to any
//! provider selected at runtime from config.

use std::future::Future;
use std::pin::Pin;

use crate::error::AbiError;
use crate::models::AdapterCapabilities;
use crate::models::ListModelsFuture;
use crate::models::ListModelsError;
use crate::request::TurnRequest;
use crate::stream::TurnStream;

/// The future returned by [`ModelAdapter::stream`].
pub type AdapterFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TurnStream, AbiError>> + Send + 'a>>;

/// A model provider adapter.
///
/// Implementations translate [`TurnRequest`] into the provider's wire
/// format, stream the response, and emit [`TurnEvent`](crate::TurnEvent)s
/// via a [`TurnStream`].
///
/// Transport setup (HTTP client, auth, WebSocket) is internal to each
/// adapter. The core does not care how the adapter talks to the provider.
///
/// ## Optional capabilities
///
/// `list_models()` is an optional capability. The default returns
/// `Err(ListModelsError::Unsupported)`. Adapters that know how to
/// list models from their provider override both `capabilities()`
/// and `list_models()`.
pub trait ModelAdapter: Send + Sync + std::fmt::Debug {
    /// Stream a turn request to the provider and return the event stream.
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_>;

    /// Provider name for telemetry and logging.
    fn provider_name(&self) -> &str;

    /// Declares which optional capabilities this adapter instance supports.
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities::default()
    }

    /// List available models from the provider.
    ///
    /// Returns `Err(ListModelsError::Unsupported)` by default.
    /// Adapters override this when the provider has a discovery endpoint.
    fn list_models(&self) -> ListModelsFuture<'_> {
        Box::pin(async { Err(ListModelsError::Unsupported) })
    }
}
