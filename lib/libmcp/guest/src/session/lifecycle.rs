use super::MessageTransport;
use state_machines::state_machine;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

pub(super) struct RuntimeResources {
    pub transport: Arc<dyn MessageTransport>,
    pub task: tokio::sync::Mutex<JoinHandle<()>>,
}

impl std::fmt::Debug for RuntimeResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeResources").finish_non_exhaustive()
    }
}

impl Drop for RuntimeResources {
    fn drop(&mut self) {
        let task = self.task.get_mut();
        if !task.is_finished() {
            task.abort();
            let transport = Arc::clone(&self.transport);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = transport.force_shutdown().await;
                });
            }
        }
    }
}

state_machine! {
    name: SessionRuntime,
    dynamic: true,
    initial: Running,
    states: [
        superstate Owned(Arc<RuntimeResources>) {
            state Running, state Closing
        },
        Closed
    ],
    final_states: [Closed],
    events {
        close {
            transition: { from: Running, to: Closing }
            transition: { from: Closing, internal: true }
        }
        finish { transition: { from: Closing, to: Closed } }
    }
}

pub(super) struct RuntimeLifecycle {
    machine: Mutex<DynamicSessionRuntime<()>>,
    pub shutdown_lock: tokio::sync::Mutex<()>,
}

impl RuntimeLifecycle {
    pub fn new(transport: Arc<dyn MessageTransport>, task: JoinHandle<()>) -> Self {
        Self {
            machine: Mutex::new(
                SessionRuntime::new(())
                    .with_owned_data(Arc::new(RuntimeResources {
                        transport,
                        task: tokio::sync::Mutex::new(task),
                    }))
                    .into_dynamic(),
            ),
            shutdown_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn accepting(&self) -> bool {
        self.machine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current_state()
            == SessionRuntimeState::Running
    }

    pub fn begin_close(&self) -> Option<Arc<RuntimeResources>> {
        let mut machine = self
            .machine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        machine.handle(SessionRuntimeEvent::Close).ok()?;
        machine.owned_data().cloned()
    }

    pub fn finish(&self) {
        let mut machine = self
            .machine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(machine.handle(SessionRuntimeEvent::Finish).is_ok());
    }
}
