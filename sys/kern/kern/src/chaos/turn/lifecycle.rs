//! Turn control only: provider streams and tool futures remain in the executor.
//! No new sample may start until the previous attempt has settled and the
//! executor has returned to a preparation boundary.

use state_machines::state_machine;

state_machine! {
    name: TurnControl,
    dynamic: true,
    initial: Preparing,
    states: [
        superstate Active {
            state Preparing, state Sampling, state SettlingTools,
            state Boundary, state Backoff, state Compacting, state Finishing
        },
        Cancelling, Completed, Cancelled, Failed, Stopped, Parked
    ],
    final_states: [Completed, Cancelled, Failed, Stopped, Parked],
    events {
        sample { transition: { from: Preparing, to: Sampling } }
        stream_ended { transition: { from: Sampling, to: SettlingTools } }
        attempt_settled {
            // Opening the provider stream can fail before any tool work exists.
            transition: { from: Sampling, to: Boundary }
            transition: { from: SettlingTools, to: Boundary }
        }
        follow_up { transition: { from: Boundary, to: Preparing } }
        repair_context { transition: { from: Boundary, to: Preparing } }
        retry {
            payload: usize,
            guards: [no_dispatched_tools],
            transition: { from: Boundary, to: Backoff }
        }
        retry_elapsed { transition: { from: Backoff, to: Preparing } }
        compact {
            transition: { from: Preparing, to: Compacting }
            transition: { from: Boundary, to: Compacting }
        }
        compacted { transition: { from: Compacting, to: Preparing } }
        compact_failed { transition: { from: Compacting, to: Boundary } }
        finish { transition: { from: Boundary, to: Finishing } }
        hook_continuation { transition: { from: Finishing, to: Preparing } }
        goal_continuation { transition: { from: Finishing, to: Preparing } }
        complete { transition: { from: Finishing, to: Completed } }
        stop { transition: { from: Active, to: Stopped } }
        park { transition: { from: Active, to: Parked } }
        fail { transition: { from: Active, to: Failed } }
        cancel { transition: { from: Active, to: Cancelling } }
        cleanup_completed { transition: { from: Cancelling, to: Cancelled } }
    }
}

impl<C, S> TurnControl<C, S> {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "state-machines guards receive event payloads by reference"
    )]
    fn no_dispatched_tools(&self, _ctx: &C, tool_calls: &usize) -> bool {
        *tool_calls == 0
    }
}

pub(super) struct TurnLifecycle {
    machine: DynamicTurnControl<()>,
}

impl Default for TurnLifecycle {
    fn default() -> Self {
        Self {
            machine: DynamicTurnControl::new(()),
        }
    }
}

impl TurnLifecycle {
    /// A failed request must not replay a prompt that predates dispatched tools.
    /// Even read-only calls are refused: their result may affect later actions.
    pub(super) fn try_retry(&mut self, tool_calls: usize) -> bool {
        self.machine
            .handle(TurnControlEvent::Retry(tool_calls))
            .is_ok()
    }

    pub(super) fn apply(&mut self, event: TurnControlEvent) {
        let before = self.machine.current_state();
        assert!(
            self.machine.handle(event).is_ok(),
            "turn executor issued an invalid transition from {before:?}"
        );
        tracing::trace!(
            ?before,
            after = ?self.machine.current_state(),
            "turn lifecycle transition"
        );
    }

    pub(super) fn cancel(&mut self) {
        if self.machine.current_state() != TurnControlState::Cancelling {
            self.apply(TurnControlEvent::Cancel);
        }
    }

    /// Called only after the executor has returned and finished its cleanup.
    /// Early error returns must not be mistaken for successful completion.
    pub(super) fn close(&mut self, cancelled: bool) {
        if self.machine.is_finished() {
            return;
        }
        if cancelled || self.machine.current_state() == TurnControlState::Cancelling {
            self.cancel();
            self.apply(TurnControlEvent::CleanupCompleted);
        } else {
            self.apply(TurnControlEvent::Fail);
        }
    }
}

#[cfg(test)]
mod tests;
