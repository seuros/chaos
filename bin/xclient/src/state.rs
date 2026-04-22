//! Application state: `ChaosWindow` struct definition and lifecycle enums.

use chaos_chassis::reducer::FrontendState;
use chaos_ipc::protocol::Op;
use tokio::sync::mpsc::UnboundedSender;

use crate::turn::TurnTemplate;

pub(crate) use chaos_chassis::reducer::SessionStatus as Status;
pub(crate) use chaos_chassis::reducer::TurnStatus as TurnState;

/// Root application state.
pub struct ChaosWindow {
    pub(super) op_tx: UnboundedSender<Op>,
    pub(super) template: TurnTemplate,
    pub(super) composer: String,
    pub(super) frontend: FrontendState,
    /// `true` once the GUI has been clamped to Claude Code MAX. Drives
    /// the palette between phosphor and anthropic families.
    pub(super) clamped: bool,
}

impl ChaosWindow {
    pub(super) fn new(template: TurnTemplate, op_tx: UnboundedSender<Op>) -> Self {
        Self {
            op_tx,
            template,
            composer: String::new(),
            frontend: FrontendState::new(),
            clamped: false,
        }
    }
}
