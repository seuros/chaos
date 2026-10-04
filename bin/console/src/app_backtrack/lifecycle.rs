use super::{BacktrackState, PendingBacktrackRollback};
use state_machines::{runtime::Parallel, state_machine};

mod priming {
    use super::*;

    state_machine! {
        name: BacktrackPriming,
        dynamic: true,
        initial: Idle,
        states: [Idle, Primed],
        events {
            prime {
                transition: { from: Idle, to: Primed }
                transition: { from: Primed, internal: true }
            }
            clear {
                transition: { from: Primed, to: Idle }
                transition: { from: Idle, internal: true }
            }
        }
    }
}

mod preview {
    use super::*;

    state_machine! {
        name: BacktrackPreview,
        dynamic: true,
        initial: Closed,
        states: [Closed, Open],
        events {
            open {
                transition: { from: Closed, to: Open }
                transition: { from: Open, internal: true }
            }
            close {
                transition: { from: Open, to: Closed }
                transition: { from: Closed, internal: true }
            }
        }
    }
}

struct RollbackData {
    request: Option<PendingBacktrackRollback>,
}

impl std::fmt::Debug for RollbackData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RollbackData(..)")
    }
}

mod rollback {
    use super::*;

    state_machine! {
        name: RollbackAcknowledgement,
        dynamic: true,
        initial: Idle,
        states: [Idle, Pending(RollbackData)],
        events {
            request {
                payload: Option<RollbackData>,
                transition: { from: Idle, to: Pending, data: own_request }
            }
            resolve { transition: { from: Pending, to: Idle } }
        }
    }

    impl<C, S> RollbackAcknowledgement<C, S> {
        fn own_request(&self, data: &mut Option<RollbackData>) -> RollbackData {
            data.take()
                .unwrap_or_else(|| unreachable!("pending rollback owns request"))
        }
    }
}

type Navigation =
    Parallel<priming::DynamicBacktrackPriming<()>, preview::DynamicBacktrackPreview<()>>;

pub(super) struct Workflow {
    regions: Parallel<Navigation, rollback::DynamicRollbackAcknowledgement<()>>,
}

impl Default for Workflow {
    fn default() -> Self {
        Self {
            regions: Parallel::new(
                Parallel::new(
                    priming::DynamicBacktrackPriming::new(()),
                    preview::DynamicBacktrackPreview::new(()),
                ),
                rollback::DynamicRollbackAcknowledgement::new(()),
            ),
        }
    }
}

impl BacktrackState {
    pub(crate) fn primed(&self) -> bool {
        self.workflow.regions.left().left().current_state()
            == priming::BacktrackPrimingState::Primed
    }

    pub(crate) fn preview_active(&self) -> bool {
        self.workflow.regions.left().right().current_state() == preview::BacktrackPreviewState::Open
    }

    pub(crate) fn prime(&mut self) {
        assert!(
            self.workflow
                .regions
                .left_mut()
                .left_mut()
                .handle(priming::BacktrackPrimingEvent::Prime)
                .is_ok()
        );
    }

    pub(crate) fn preview(&mut self) {
        assert!(
            self.workflow
                .regions
                .left_mut()
                .right_mut()
                .handle(preview::BacktrackPreviewEvent::Open)
                .is_ok()
        );
    }

    pub(crate) fn unprime(&mut self) {
        assert!(
            self.workflow
                .regions
                .left_mut()
                .left_mut()
                .handle(priming::BacktrackPrimingEvent::Clear)
                .is_ok()
        );
    }

    pub(crate) fn close_preview(&mut self) {
        assert!(
            self.workflow
                .regions
                .left_mut()
                .right_mut()
                .handle(preview::BacktrackPreviewEvent::Close)
                .is_ok()
        );
    }

    pub(crate) fn rollback_pending(&self) -> bool {
        self.workflow.regions.right().pending_data().is_some()
    }

    pub(crate) fn request_rollback(&mut self, pending: PendingBacktrackRollback) -> bool {
        self.workflow
            .regions
            .right_mut()
            .handle(rollback::RollbackAcknowledgementEvent::Request(Some(
                RollbackData {
                    request: Some(pending),
                },
            )))
            .is_ok()
    }

    pub(crate) fn take_rollback(&mut self) -> Option<PendingBacktrackRollback> {
        let request = self
            .workflow
            .regions
            .right_mut()
            .pending_data_mut()?
            .request
            .take();
        assert!(
            self.workflow
                .regions
                .right_mut()
                .handle(rollback::RollbackAcknowledgementEvent::Resolve)
                .is_ok()
        );
        request
    }
}

#[cfg(test)]
mod tests;
