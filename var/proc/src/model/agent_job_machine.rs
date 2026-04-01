pub(crate) mod job {
    use crate::AgentJobStatus;
    use state_machines::state_machine;

    // Agent job lifecycle: Pending → Running → Completed/Failed/Cancelled
    // Cancellation is allowed from Pending or Running.
    state_machine! {
        name: AgentJobLifecycle,
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
    pub(crate) struct AgentJobWorkflow {
        machine: DynamicAgentJobLifecycle<()>,
    }

    impl AgentJobWorkflow {
        pub(crate) fn new() -> Self {
            Self {
                machine: DynamicAgentJobLifecycle::new(()),
            }
        }

        /// Reconstruct the workflow at a known persisted state by replaying
        /// the minimal events needed to reach it.
        pub(crate) fn from_status(status: AgentJobStatus) -> Self {
            let mut wf = Self::new();
            match status {
                AgentJobStatus::Pending => {}
                AgentJobStatus::Running => {
                    wf.start();
                }
                AgentJobStatus::Completed => {
                    wf.start();
                    wf.complete();
                }
                AgentJobStatus::Failed => {
                    wf.start();
                    wf.fail();
                }
                AgentJobStatus::Cancelled => {
                    wf.cancel();
                }
            }
            wf
        }

        pub(crate) fn start(&mut self) -> bool {
            self.machine
                .handle(AgentJobLifecycleEvent::Start)
                .is_ok()
        }

        pub(crate) fn complete(&mut self) -> bool {
            self.machine
                .handle(AgentJobLifecycleEvent::Complete)
                .is_ok()
        }

        pub(crate) fn fail(&mut self) -> bool {
            self.machine
                .handle(AgentJobLifecycleEvent::Fail)
                .is_ok()
        }

        pub(crate) fn cancel(&mut self) -> bool {
            self.machine
                .handle(AgentJobLifecycleEvent::Cancel)
                .is_ok()
        }

        #[cfg(test)]
        pub(crate) fn current_state(&self) -> &str {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn happy_path_pending_to_completed() {
            let mut wf = AgentJobWorkflow::new();
            assert_eq!(wf.current_state(), "Pending");

            assert!(wf.start());
            assert_eq!(wf.current_state(), "Running");

            assert!(wf.complete());
            assert_eq!(wf.current_state(), "Completed");
        }

        #[test]
        fn can_fail_from_running() {
            let mut wf = AgentJobWorkflow::new();
            wf.start();

            assert!(wf.fail());
            assert_eq!(wf.current_state(), "Failed");
        }

        #[test]
        fn cancel_from_pending() {
            let mut wf = AgentJobWorkflow::new();
            assert!(wf.cancel());
            assert_eq!(wf.current_state(), "Cancelled");
        }

        #[test]
        fn cancel_from_running() {
            let mut wf = AgentJobWorkflow::new();
            wf.start();
            assert!(wf.cancel());
            assert_eq!(wf.current_state(), "Cancelled");
        }

        #[test]
        fn cannot_complete_from_pending() {
            let mut wf = AgentJobWorkflow::new();
            assert!(!wf.complete());
            assert_eq!(wf.current_state(), "Pending");
        }

        #[test]
        fn from_status_roundtrip() {
            let cases = [
                (AgentJobStatus::Pending, "Pending"),
                (AgentJobStatus::Running, "Running"),
                (AgentJobStatus::Completed, "Completed"),
                (AgentJobStatus::Failed, "Failed"),
                (AgentJobStatus::Cancelled, "Cancelled"),
            ];
            for (status, expected) in cases {
                let wf = AgentJobWorkflow::from_status(status);
                assert_eq!(wf.current_state(), expected);
            }
        }
    }
}

pub(crate) mod item {
    use crate::AgentJobItemStatus;
    use state_machines::state_machine;

    // Agent job item lifecycle: Pending → Running → Completed/Failed
    // Items can be retried: Running → Pending.
    state_machine! {
        name: AgentJobItemLifecycle,
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
    pub(crate) struct AgentJobItemWorkflow {
        machine: DynamicAgentJobItemLifecycle<()>,
    }

    impl AgentJobItemWorkflow {
        pub(crate) fn new() -> Self {
            Self {
                machine: DynamicAgentJobItemLifecycle::new(()),
            }
        }

        /// Reconstruct the workflow at a known persisted state.
        pub(crate) fn from_status(status: AgentJobItemStatus) -> Self {
            let mut wf = Self::new();
            match status {
                AgentJobItemStatus::Pending => {}
                AgentJobItemStatus::Running => {
                    wf.start();
                }
                AgentJobItemStatus::Completed => {
                    wf.start();
                    wf.complete();
                }
                AgentJobItemStatus::Failed => {
                    wf.start();
                    wf.fail();
                }
            }
            wf
        }

        pub(crate) fn start(&mut self) -> bool {
            self.machine
                .handle(AgentJobItemLifecycleEvent::Start)
                .is_ok()
        }

        pub(crate) fn complete(&mut self) -> bool {
            self.machine
                .handle(AgentJobItemLifecycleEvent::Complete)
                .is_ok()
        }

        pub(crate) fn fail(&mut self) -> bool {
            self.machine
                .handle(AgentJobItemLifecycleEvent::Fail)
                .is_ok()
        }

        pub(crate) fn retry(&mut self) -> bool {
            self.machine
                .handle(AgentJobItemLifecycleEvent::Retry)
                .is_ok()
        }

        #[cfg(test)]
        pub(crate) fn current_state(&self) -> &str {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn happy_path() {
            let mut wf = AgentJobItemWorkflow::new();
            assert_eq!(wf.current_state(), "Pending");

            assert!(wf.start());
            assert_eq!(wf.current_state(), "Running");

            assert!(wf.complete());
            assert_eq!(wf.current_state(), "Completed");
        }

        #[test]
        fn retry_returns_to_pending() {
            let mut wf = AgentJobItemWorkflow::new();
            wf.start();

            assert!(wf.retry());
            assert_eq!(wf.current_state(), "Pending");

            assert!(wf.start());
            assert_eq!(wf.current_state(), "Running");
        }

        #[test]
        fn cannot_retry_from_pending() {
            let mut wf = AgentJobItemWorkflow::new();
            assert!(!wf.retry());
            assert_eq!(wf.current_state(), "Pending");
        }
    }
}
