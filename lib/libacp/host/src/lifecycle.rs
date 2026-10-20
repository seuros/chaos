//! Synchronous lifecycle decisions only: no awaits or external effects in transitions.
//! Tokens wake waiters; these machines, under their owner's lock, authorize work.

mod connection;
mod session;
mod turn;

pub(crate) use connection::Connection;
pub(crate) use session::Session;
pub(crate) use turn::TurnState;
