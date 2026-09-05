pub(crate) mod job {
    use crate::ChildAgentJobStatus;
    use state_machines::state_machine;

    // Agent job lifecycle: Pending → Running → Completed/Failed/Cancelled
    // Cancellation is allowed from Pending or Running.
    state_machine! {
        name: ChildAgentJobLifecycle,
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
    pub(crate) struct ChildAgentJobWorkflow {
        machine: DynamicChildAgentJobLifecycle<()>,
    }

    impl ChildAgentJobWorkflow {
        pub(crate) fn new() -> Self {
            Self {
                machine: DynamicChildAgentJobLifecycle::new(()),
            }
        }

        /// Reconstruct the workflow at a known persisted state by replaying
        /// the minimal events needed to reach it.
        pub(crate) fn from_status(status: ChildAgentJobStatus) -> Self {
            let mut wf = Self::new();
            match status {
                ChildAgentJobStatus::Pending => {}
                ChildAgentJobStatus::Running => {
                    wf.start();
                }
                ChildAgentJobStatus::Completed => {
                    wf.start();
                    wf.complete();
                }
                ChildAgentJobStatus::Failed => {
                    wf.start();
                    wf.fail();
                }
                ChildAgentJobStatus::Cancelled => {
                    wf.cancel();
                }
            }
            wf
        }

        pub(crate) fn start(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobLifecycleEvent::Start)
                .is_ok()
        }

        pub(crate) fn complete(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobLifecycleEvent::Complete)
                .is_ok()
        }

        pub(crate) fn fail(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobLifecycleEvent::Fail)
                .is_ok()
        }

        pub(crate) fn cancel(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobLifecycleEvent::Cancel)
                .is_ok()
        }

        #[cfg(test)]
        pub(crate) fn current_state(&self) -> ChildAgentJobLifecycleState {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn workflow_transitions_and_persisted_status_replay() {
            let mut wf = ChildAgentJobWorkflow::new();
            assert_eq!(wf.current_state(), ChildAgentJobLifecycleState::Pending);

            assert!(wf.start());
            assert_eq!(wf.current_state(), ChildAgentJobLifecycleState::Running);

            assert!(wf.complete());
            assert_eq!(wf.current_state(), ChildAgentJobLifecycleState::Completed);

            let mut wf = ChildAgentJobWorkflow::new();
            wf.start();
            assert!(wf.fail());
            assert_eq!(wf.current_state(), ChildAgentJobLifecycleState::Failed);

            let mut wf = ChildAgentJobWorkflow::new();
            assert!(wf.cancel());
            assert_eq!(wf.current_state(), ChildAgentJobLifecycleState::Cancelled);

            let mut wf = ChildAgentJobWorkflow::new();
            wf.start();
            assert!(wf.cancel());
            assert_eq!(wf.current_state(), ChildAgentJobLifecycleState::Cancelled);

            let mut wf = ChildAgentJobWorkflow::new();
            assert!(!wf.complete());
            assert_eq!(wf.current_state(), ChildAgentJobLifecycleState::Pending);

            let cases = [
                (
                    ChildAgentJobStatus::Pending,
                    ChildAgentJobLifecycleState::Pending,
                ),
                (
                    ChildAgentJobStatus::Running,
                    ChildAgentJobLifecycleState::Running,
                ),
                (
                    ChildAgentJobStatus::Completed,
                    ChildAgentJobLifecycleState::Completed,
                ),
                (
                    ChildAgentJobStatus::Failed,
                    ChildAgentJobLifecycleState::Failed,
                ),
                (
                    ChildAgentJobStatus::Cancelled,
                    ChildAgentJobLifecycleState::Cancelled,
                ),
            ];
            for (status, expected) in cases {
                let wf = ChildAgentJobWorkflow::from_status(status);
                assert_eq!(wf.current_state(), expected);
            }
        }
    }
}

pub(crate) mod item {
    use crate::ChildAgentJobItemStatus;
    use state_machines::state_machine;

    // Agent job item lifecycle: Pending → Running → Completed/Failed
    // Items can be retried: Running → Pending.
    state_machine! {
        name: ChildAgentJobItemLifecycle,
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
    pub(crate) struct ChildAgentJobItemWorkflow {
        machine: DynamicChildAgentJobItemLifecycle<()>,
    }

    impl ChildAgentJobItemWorkflow {
        pub(crate) fn new() -> Self {
            Self {
                machine: DynamicChildAgentJobItemLifecycle::new(()),
            }
        }

        /// Reconstruct the workflow at a known persisted state.
        pub(crate) fn from_status(status: ChildAgentJobItemStatus) -> Self {
            let mut wf = Self::new();
            match status {
                ChildAgentJobItemStatus::Pending => {}
                ChildAgentJobItemStatus::Running => {
                    wf.start();
                }
                ChildAgentJobItemStatus::Completed => {
                    wf.start();
                    wf.complete();
                }
                ChildAgentJobItemStatus::Failed => {
                    wf.start();
                    wf.fail();
                }
            }
            wf
        }

        pub(crate) fn start(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobItemLifecycleEvent::Start)
                .is_ok()
        }

        pub(crate) fn complete(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobItemLifecycleEvent::Complete)
                .is_ok()
        }

        pub(crate) fn fail(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobItemLifecycleEvent::Fail)
                .is_ok()
        }

        pub(crate) fn retry(&mut self) -> bool {
            self.machine
                .handle(ChildAgentJobItemLifecycleEvent::Retry)
                .is_ok()
        }

        #[cfg(test)]
        pub(crate) fn current_state(&self) -> ChildAgentJobItemLifecycleState {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn workflow_transitions_retry_and_persisted_status_replay() {
            let mut wf = ChildAgentJobItemWorkflow::new();
            assert_eq!(wf.current_state(), ChildAgentJobItemLifecycleState::Pending);

            assert!(wf.start());
            assert_eq!(wf.current_state(), ChildAgentJobItemLifecycleState::Running);

            assert!(wf.complete());
            assert_eq!(
                wf.current_state(),
                ChildAgentJobItemLifecycleState::Completed
            );

            let mut wf = ChildAgentJobItemWorkflow::new();
            wf.start();
            assert!(wf.retry());
            assert_eq!(wf.current_state(), ChildAgentJobItemLifecycleState::Pending);

            assert!(wf.start());
            assert_eq!(wf.current_state(), ChildAgentJobItemLifecycleState::Running);

            let mut wf = ChildAgentJobItemWorkflow::new();
            assert!(!wf.retry());
            assert_eq!(wf.current_state(), ChildAgentJobItemLifecycleState::Pending);

            let cases = [
                (
                    ChildAgentJobItemStatus::Pending,
                    ChildAgentJobItemLifecycleState::Pending,
                ),
                (
                    ChildAgentJobItemStatus::Running,
                    ChildAgentJobItemLifecycleState::Running,
                ),
                (
                    ChildAgentJobItemStatus::Completed,
                    ChildAgentJobItemLifecycleState::Completed,
                ),
                (
                    ChildAgentJobItemStatus::Failed,
                    ChildAgentJobItemLifecycleState::Failed,
                ),
            ];
            for (status, expected) in cases {
                let wf = ChildAgentJobItemWorkflow::from_status(status);
                assert_eq!(wf.current_state(), expected);
            }
        }
    }
}
