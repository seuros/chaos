use state_machines::state_machine;

state_machine! {
    name: ConnectionLifecycle,
    dynamic: true,
    initial: Uninitialized,
    states: [Uninitialized, Open, Closing, Cleaning, Closed, CleanupUnconfirmed],
    final_states: [Closed, CleanupUnconfirmed],
    events {
        accept { transition: { from: Uninitialized, to: Open } }
        close {
            transition: { from: Uninitialized, to: Closing }
            transition: { from: Open, to: Closing }
        }
        clean {
            transition: { from: Uninitialized, to: Cleaning }
            transition: { from: Open, to: Cleaning }
            transition: { from: Closing, to: Cleaning }
        }
        confirm { transition: { from: Cleaning, to: Closed } }
        unconfirmed { transition: { from: Cleaning, to: CleanupUnconfirmed } }
    }
}

pub(crate) struct Connection {
    pub failure: Option<&'static str>,
    machine: DynamicConnectionLifecycle<()>,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            machine: ConnectionLifecycle::new(()).into_dynamic(),
            failure: None,
        }
    }
}

impl Connection {
    pub fn initialize(&mut self) -> bool {
        self.machine
            .handle(ConnectionLifecycleEvent::Accept)
            .is_ok()
    }

    pub fn is_open(&self) -> bool {
        self.machine.current_state() == ConnectionLifecycleState::Open
    }

    pub fn close(&mut self) {
        // Repeated close/drop must not undo cleanup or revive a terminal connection.
        let _ = self.machine.handle(ConnectionLifecycleEvent::Close);
    }

    pub fn begin_cleanup(&mut self) -> bool {
        self.machine.handle(ConnectionLifecycleEvent::Clean).is_ok()
    }

    pub fn finish_cleanup(&mut self, confirmed: bool) {
        let event = if confirmed {
            ConnectionLifecycleEvent::Confirm
        } else {
            ConnectionLifecycleEvent::Unconfirmed
        };
        let _ = self.machine.handle(event);
    }

    pub fn cleanup_confirmed(&self) -> bool {
        self.machine.current_state() == ConnectionLifecycleState::Closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_and_cleanup_are_once_only_and_terminal() {
        let mut connection = Connection::default();
        assert!(!connection.is_open());
        assert!(connection.initialize());
        assert!(!connection.initialize());
        assert!(connection.is_open());
        connection.close();
        connection.close();
        assert!(!connection.is_open());
        assert!(!connection.initialize());
        assert!(connection.begin_cleanup());
        assert!(!connection.begin_cleanup());
        connection.finish_cleanup(true);
        connection.finish_cleanup(false);
        connection.close();
        assert!(connection.cleanup_confirmed());
        assert!(!connection.begin_cleanup());
        assert!(!connection.initialize());
    }

    #[test]
    fn cleanup_before_initialization_and_unconfirmed_cleanup_cannot_reopen() {
        let mut connection = Connection::default();
        assert!(connection.begin_cleanup());
        assert!(!connection.initialize());
        connection.finish_cleanup(false);
        connection.finish_cleanup(true);
        connection.close();
        assert!(!connection.cleanup_confirmed());
        assert!(!connection.begin_cleanup());
        assert!(!connection.initialize());
    }
}
