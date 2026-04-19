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
        pub(crate) fn new() -> Self {
            Self {
                machine: DynamicMinionJobLifecycle::new(()),
            }
        }

        /// Reconstruct the workflow at a known persisted state by replaying
        /// the minimal events needed to reach it.
        pub(crate) fn from_status(status: MinionJobStatus) -> Self {
            let mut wf = Self::new();
            match status {
                MinionJobStatus::Pending => {}
                MinionJobStatus::Running => {
                    wf.start();
                }
                MinionJobStatus::Completed => {
                    wf.start();
                    wf.complete();
                }
                MinionJobStatus::Failed => {
                    wf.start();
                    wf.fail();
                }
                MinionJobStatus::Cancelled => {
                    wf.cancel();
                }
            }
            wf
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
        pub(crate) fn current_state(&self) -> &str {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn happy_path_pending_to_completed() {
            let mut wf = MinionJobWorkflow::new();
            assert_eq!(wf.current_state(), "Pending");

            assert!(wf.start());
            assert_eq!(wf.current_state(), "Running");

            assert!(wf.complete());
            assert_eq!(wf.current_state(), "Completed");
        }

        #[test]
        fn can_fail_from_running() {
            let mut wf = MinionJobWorkflow::new();
            wf.start();

            assert!(wf.fail());
            assert_eq!(wf.current_state(), "Failed");
        }

        #[test]
        fn cancel_from_pending() {
            let mut wf = MinionJobWorkflow::new();
            assert!(wf.cancel());
            assert_eq!(wf.current_state(), "Cancelled");
        }

        #[test]
        fn cancel_from_running() {
            let mut wf = MinionJobWorkflow::new();
            wf.start();
            assert!(wf.cancel());
            assert_eq!(wf.current_state(), "Cancelled");
        }

        #[test]
        fn cannot_complete_from_pending() {
            let mut wf = MinionJobWorkflow::new();
            assert!(!wf.complete());
            assert_eq!(wf.current_state(), "Pending");
        }

        #[test]
        fn from_status_roundtrip() {
            let cases = [
                (MinionJobStatus::Pending, "Pending"),
                (MinionJobStatus::Running, "Running"),
                (MinionJobStatus::Completed, "Completed"),
                (MinionJobStatus::Failed, "Failed"),
                (MinionJobStatus::Cancelled, "Cancelled"),
            ];
            for (status, expected) in cases {
                let wf = MinionJobWorkflow::from_status(status);
                assert_eq!(wf.current_state(), expected);
            }
        }
    }
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

        /// Reconstruct the workflow at a known persisted state.
        pub(crate) fn from_status(status: MinionJobItemStatus) -> Self {
            let mut wf = Self::new();
            match status {
                MinionJobItemStatus::Pending => {}
                MinionJobItemStatus::Running => {
                    wf.start();
                }
                MinionJobItemStatus::Completed => {
                    wf.start();
                    wf.complete();
                }
                MinionJobItemStatus::Failed => {
                    wf.start();
                    wf.fail();
                }
            }
            wf
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
        pub(crate) fn current_state(&self) -> &str {
            self.machine.current_state()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn happy_path() {
            let mut wf = MinionJobItemWorkflow::new();
            assert_eq!(wf.current_state(), "Pending");

            assert!(wf.start());
            assert_eq!(wf.current_state(), "Running");

            assert!(wf.complete());
            assert_eq!(wf.current_state(), "Completed");
        }

        #[test]
        fn retry_returns_to_pending() {
            let mut wf = MinionJobItemWorkflow::new();
            wf.start();

            assert!(wf.retry());
            assert_eq!(wf.current_state(), "Pending");

            assert!(wf.start());
            assert_eq!(wf.current_state(), "Running");
        }

        #[test]
        fn cannot_retry_from_pending() {
            let mut wf = MinionJobItemWorkflow::new();
            assert!(!wf.retry());
            assert_eq!(wf.current_state(), "Pending");
        }
    }
}
