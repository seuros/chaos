//! `chaos-synopsis` — declarative asynchronous workflows for ChaOS.
//!
//! A [`Synopsis`] describes control flow while an [`ActionExecutor`] supplies
//! the asynchronous implementation of each [`Node::Action`] leaf. [`Runner`]
//! compiles the ChaOS-owned definition into an [`mcp_host::bt`] behavior tree.
//! Upstream scoped jobs and halt semantics manage concurrency, branch
//! abandonment, and cancellation without detached action tasks.
//!
//! The initial API intentionally supports only acyclic workflows. This keeps
//! every action ID single-use and makes cancellation semantics explicit before
//! adding loops or persistent execution.

mod model;
mod runner;
pub mod runs;
pub mod tree;

pub use model::{ActionId, ActionOutcome, CompositeKind, Node, Outcome, Synopsis, ValidationError};
pub use runner::{ActionExecutor, ActionFuture, RunError, Runner};
