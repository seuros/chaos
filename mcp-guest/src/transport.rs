use std::future::Future;
use std::pin::Pin;

use crate::error::GuestError;
use crate::protocol::JsonRpcMessage;

pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, GuestError>> + Send + 'a>>;

pub trait MessageTransport: Send + Sync + 'static {
    fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()>;
    fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage>;
    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()>;
}

#[cfg(feature = "stdio")]
pub mod stdio;

#[cfg(feature = "stdio")]
pub use stdio::StdioTransport;

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "http")]
pub use http::HttpTransport;
