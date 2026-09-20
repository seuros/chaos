pub(crate) mod job {
    use crate::MinionJobStatus;
    use state_machines::state_machine;

    // Agent job lifecycle: Pending → Running → Completed/Failed/Cancelled
    // Cancellation is allowed from Pending or Running.
    state_machine! {
        name: MinionJobLifecycle,
        dynamic: true,
        initial: Pending,
        states: [Pending, Running, Completed, Failed, Cancelled],
        events {
            start {
                transition: { from: Pending, to: Running }
            }
            complete {
                transition: { from: Running, to: Completed }
            }
            fail {
                transition: { from: Running, to: Failed }
            }
            cancel {
                transition: { from: Pending, to: Cancelled }
                transition: { from: Running, to: Cancelled }
            }
        }
    }

    #[derive(Debug)]
    pub(crate) struct MinionJobWorkflow {
        machine: DynamicMinionJobLifecycle<()>,
    }

    impl MinionJobWorkflow {
        #[cfg(test)]
        pub(crate) fn new() -> Self {
            Self {
                machine: DynamicMinionJobLifecycle::new(()),
            }
        }

        /// Restore persisted state without replaying transitions or callbacks.
        pub(crate) fn from_status(status: MinionJobStatus) -> Self {
            let state = match status {
                MinionJobStatus::Pending => MinionJobLifecycleState::Pending,
                MinionJobStatus::Running => MinionJobLifecycleState::Running,
                MinionJobStatus::Completed => MinionJobLifecycleState::Completed,
                MinionJobStatus::Failed => MinionJobLifecycleState::Failed,
                MinionJobStatus::Cancelled => MinionJobLifecycleState::Cancelled,
            };
            Self {
                machine: DynamicMinionJobLifecycle::new_init_state((), state),
            }
        }

        pub(crate) fn start(&mut self) -> bool {
            self.machine.handle(MinionJobLifecycleEvent::Start).is_ok()
        }

        pub(crate) fn complete(&mut self) -> bool {
            self.machine
                .handle(MinionJobLifecycleEvent::Complete)
                .is_ok()
        }

        pub(crate) fn fail(&mut self) -> bool {
            self.machine.handle(MinionJobLifecycleEvent::Fail).is_ok()
        }

        pub(crate) fn cancel(&mut self) -> bool {
            self.machine.handle(MinionJobLifecycleEvent::Cancel).is_ok()
        }

        #[cfg(test)]
        pub(crate) fn current_state(&self) -> MinionJobLifecycleState {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests;
}

pub(crate) mod item {
    use crate::MinionJobItemStatus;
    use state_machines::state_machine;

    // Agent job item lifecycle: Pending → Running → Completed/Failed
    // Items can be retried: Running → Pending.
    state_machine! {
        name: MinionJobItemLifecycle,
        dynamic: true,
        initial: Pending,
        states: [Pending, Running, Completed, Failed],
        events {
            start {
                transition: { from: Pending, to: Running }
            }
            complete {
                transition: { from: Running, to: Completed }
            }
            fail {
                transition: { from: Running, to: Failed }
            }
            retry {
                transition: { from: Running, to: Pending }
            }
        }
    }

    #[derive(Debug)]
    pub(crate) struct MinionJobItemWorkflow {
        machine: DynamicMinionJobItemLifecycle<()>,
    }

    impl MinionJobItemWorkflow {
        pub(crate) fn new() -> Self {
            Self {
                machine: DynamicMinionJobItemLifecycle::new(()),
            }
        }

        /// Restore persisted state without replaying transitions or callbacks.
        pub(crate) fn from_status(status: MinionJobItemStatus) -> Self {
            let state = match status {
                MinionJobItemStatus::Pending => MinionJobItemLifecycleState::Pending,
                MinionJobItemStatus::Running => MinionJobItemLifecycleState::Running,
                MinionJobItemStatus::Completed => MinionJobItemLifecycleState::Completed,
                MinionJobItemStatus::Failed => MinionJobItemLifecycleState::Failed,
            };
            Self {
                machine: DynamicMinionJobItemLifecycle::new_init_state((), state),
            }
        }

        pub(crate) fn start(&mut self) -> bool {
            self.machine
                .handle(MinionJobItemLifecycleEvent::Start)
                .is_ok()
        }

        pub(crate) fn complete(&mut self) -> bool {
            self.machine
                .handle(MinionJobItemLifecycleEvent::Complete)
                .is_ok()
        }

        pub(crate) fn fail(&mut self) -> bool {
            self.machine
                .handle(MinionJobItemLifecycleEvent::Fail)
                .is_ok()
        }

        pub(crate) fn retry(&mut self) -> bool {
            self.machine
                .handle(MinionJobItemLifecycleEvent::Retry)
                .is_ok()
        }

        #[cfg(test)]
        pub(crate) fn current_state(&self) -> MinionJobItemLifecycleState {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests;
}
