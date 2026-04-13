//! Application state: `ChaosWindow` struct definition and lifecycle enums.

use std::collections::HashMap;

use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::TokenUsage;
use tokio::sync::mpsc::UnboundedSender;

use crate::chat::ChatEntry;
use crate::turn::TurnTemplate;

/// Lifecycle state of the kernel session as observed by the GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    /// We haven't seen `SessionConfigured` yet.
    Booting,
    /// Kernel is alive and ready to accept turns.
    Ready,
    /// Kernel emitted `ShutdownComplete` or the event channel closed.
    Shutdown,
}

/// Whether the composer is allowed to submit a new turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnState {
    /// No turn outstanding — composer is enabled.
    Idle,
    /// A turn was submitted, waiting for `TurnComplete` or an error.
    InFlight,
}

/// Root application state.
pub struct ChaosWindow {
    pub(super) op_tx: UnboundedSender<Op>,
    pub(super) template: TurnTemplate,
    pub(super) composer: String,
    pub(super) transcript: Vec<ChatEntry>,
    pub(super) status: Status,
    pub(super) turn: TurnState,
    /// Latest token usage (if the kernel has reported any). Rendered in the
    /// header as a compact `in/out/total` triple.
    pub(super) token_usage: Option<TokenUsage>,
    /// Streaming-delta bookkeeping: item_id → transcript index of the
    /// in-progress `Agent` / `Reasoning` entry. Keyed by item_id because the
    /// kernel may interleave deltas from multiple items within one turn.
    pub(super) pending_streams: HashMap<String, usize>,
    /// Exec / tool call bookkeeping: call_id → transcript index so the
    /// matching end-event can flip the entry from "running" to "done".
    pub(super) pending_calls: HashMap<String, usize>,
    /// `true` once the GUI has been clamped to Claude Code MAX. Drives
    /// the palette: [`PHOSPHOR`] when false, [`ANTHROPIC`] when true.
    /// Owned as a plain `bool` rather than console's process-global
    /// `AtomicBool` because iced state is single-threaded — the GUI is the
    /// only reader/writer in this process.
    pub(super) clamped: bool,
}

impl ChaosWindow {
    pub(super) fn new(template: TurnTemplate, op_tx: UnboundedSender<Op>) -> Self {
        Self {
            op_tx,
            template,
            composer: String::new(),
            transcript: Vec::new(),
            status: Status::Booting,
            turn: TurnState::Idle,
            token_usage: None,
            pending_streams: HashMap::new(),
            pending_calls: HashMap::new(),
            clamped: false,
        }
    }
}
