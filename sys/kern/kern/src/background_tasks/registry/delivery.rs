use state_machines::{runtime::Parallel, state_machine};

state_machine! {
    name: TaskOrigin,
    dynamic: true,
    initial: OriginPending,
    states: [OriginPending, OriginCommitted],
    events {
        bind {
            transition: { from: OriginPending, internal: true }
            transition: { from: OriginCommitted, to: OriginPending }
        }
        commit {
            transition: { from: OriginPending, to: OriginCommitted }
            transition: { from: OriginCommitted, internal: true }
        }
    }
}

state_machine! {
    name: TaskReceipt,
    dynamic: true,
    initial: UnreadResult,
    states: [UnreadResult, ReadResult],
    final_states: [ReadResult],
    events {
        deliver { transition: { from: UnreadResult, to: ReadResult } }
    }
}

pub(super) type TaskDelivery = Parallel<DynamicTaskOrigin<()>, DynamicTaskReceipt<()>>;

pub(super) enum TaskDeliveryEvent {
    BindOrigin,
    CommitOrigin,
    Deliver,
}

pub(super) fn restore(ready: bool, delivered: bool) -> TaskDelivery {
    Parallel::new(
        DynamicTaskOrigin::new_init_state(
            (),
            if ready {
                TaskOriginState::OriginCommitted
            } else {
                TaskOriginState::OriginPending
            },
        ),
        DynamicTaskReceipt::new_init_state(
            (),
            if delivered {
                TaskReceiptState::ReadResult
            } else {
                TaskReceiptState::UnreadResult
            },
        ),
    )
}

pub(super) fn ready(machine: &TaskDelivery) -> bool {
    machine.left().current_state() == TaskOriginState::OriginCommitted
}

pub(super) fn delivered(machine: &TaskDelivery) -> bool {
    machine.right().is_finished()
}

pub(super) fn apply(machine: &mut TaskDelivery, event: TaskDeliveryEvent) {
    let result = match event {
        TaskDeliveryEvent::BindOrigin => machine.left_mut().handle(TaskOriginEvent::Bind),
        TaskDeliveryEvent::CommitOrigin => machine.left_mut().handle(TaskOriginEvent::Commit),
        TaskDeliveryEvent::Deliver if delivered(machine) => return,
        TaskDeliveryEvent::Deliver => machine.right_mut().handle(TaskReceiptEvent::Deliver),
    };
    assert!(result.is_ok(), "task delivery transition is valid");
}
