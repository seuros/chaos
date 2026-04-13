//! Protocol event dispatch: routes `EventMsg` values to per-event handlers.

pub(super) mod approvals;
pub(super) mod dispatch;
pub(super) mod exec;
pub(super) mod lifecycle;
pub(super) mod state_handlers;
pub(super) mod tools;
