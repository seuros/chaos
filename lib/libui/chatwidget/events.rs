//! Protocol event dispatch for `ChatWidget`.
//!
//! This module collects the methods that consume `EventMsg` values from the
//! chaos-kern event stream and translate them into widget state mutations.
//! The public surface is `handle_codex_event` and `handle_codex_event_replay`
//! which route through the private `dispatch_event_msg` dispatcher.

mod approval_handlers;
mod immediate_handlers;
mod keyboard_ui;
mod protocol_dispatch;
