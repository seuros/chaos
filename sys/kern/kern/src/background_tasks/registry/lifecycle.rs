use super::task::TaskData;
use chaos_ipc::background_tasks::WakePolicy;
use state_machines::state_machine;

state_machine! {
    name: TaskExecution,
    dynamic: true,
    initial: Submitting,
    states: [
        superstate Record(TaskData) {
            superstate Live { state Submitting, state Running, state InputRequired },
            state Succeeded, state Failed, state Cancelled, state Lost, state SubmissionUnknown,
        },
    ],
    final_states: [Failed, Cancelled, Lost, SubmissionUnknown],
    events {
        run { transition: { from: Live, to: Running } }
        input { transition: { from: Live, to: InputRequired } }
        succeed { transition: { from: Live, to: Succeeded } }
        fail { transition: { from: Live, to: Failed } }
        cancel { transition: { from: Live, to: Cancelled } }
        lost { transition: { from: Live, to: Lost } }
        unknown { transition: { from: Live, to: SubmissionUnknown } }
        withdraw_recovery {
            payload: bool,
            guards: [session_local_undelivered],
            transition: { from: Succeeded, to: Running }
        }
        revoke_recovery {
            payload: bool,
            guards: [session_local_undelivered],
            transition: { from: Succeeded, to: Cancelled }
        }
    }
}

impl<C, S> TaskExecution<C, S> {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "state-machines guards receive event payloads by reference"
    )]
    fn session_local_undelivered(&self, _ctx: &C, permitted: &bool) -> bool {
        *permitted
    }
}

state_machine! {
    name: WakeAdmission,
    dynamic: true,
    initial: Enabled,
    states: [Enabled, Interrupted, Closed],
    events {
        enable {
            transition: { from: Enabled, internal: true }
            transition: { from: Interrupted, to: Enabled }
        }
        interrupt {
            transition: { from: Enabled, to: Interrupted }
            transition: { from: Interrupted, internal: true }
        }
        close {
            transition: { from: Enabled, to: Closed }
            transition: { from: Interrupted, to: Closed }
            transition: { from: Closed, internal: true }
        }
    }
}

pub(super) fn restore_policy(policy: WakePolicy) -> DynamicWakeAdmission<()> {
    let state = match policy {
        WakePolicy::Enabled => WakeAdmissionState::Enabled,
        WakePolicy::Interrupted => WakeAdmissionState::Interrupted,
        WakePolicy::Closed => WakeAdmissionState::Closed,
    };
    DynamicWakeAdmission::new_init_state((), state)
}

pub(super) fn policy(machine: &DynamicWakeAdmission<()>) -> WakePolicy {
    match machine.current_state() {
        WakeAdmissionState::Enabled => WakePolicy::Enabled,
        WakeAdmissionState::Interrupted => WakePolicy::Interrupted,
        WakeAdmissionState::Closed => WakePolicy::Closed,
    }
}

pub(super) fn set_policy(machine: &mut DynamicWakeAdmission<()>, next: WakePolicy) -> bool {
    let event = match next {
        WakePolicy::Enabled => WakeAdmissionEvent::Enable,
        WakePolicy::Interrupted => WakeAdmissionEvent::Interrupt,
        WakePolicy::Closed => WakeAdmissionEvent::Close,
    };
    machine.handle(event).is_ok()
}
