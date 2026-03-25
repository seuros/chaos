pub mod connect;
pub mod error;
pub mod handler;
pub mod protocol;
pub mod runtime;
pub mod session;
pub mod transport;

#[cfg(feature = "http")]
pub use connect::{HttpBuilder, http};
#[cfg(feature = "stdio")]
pub use connect::{StdioBuilder, stdio};
pub use error::GuestError;
pub use handler::{
    ClientHandler, ClientHandlerFuture, ClientHandlerResultFuture, NoopClientHandler,
};
pub use protocol::*;
pub use session::McpSession;
