use state_machines::state_machine;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::warn;

#[derive(Debug)]
pub(super) struct ActorTask(pub tokio::sync::Mutex<JoinHandle<()>>);

impl Drop for ActorTask {
    fn drop(&mut self) {
        self.0.get_mut().abort();
    }
}

state_machine! {
    name: McpActor,
    dynamic: true,
    initial: Running,
    states: [
        superstate Owned(Arc<ActorTask>) {
            state Running, state Draining
        },
        Stopped
    ],
    final_states: [Stopped],
    events {
        drain {
            transition: { from: Running, to: Draining }
            transition: { from: Draining, internal: true }
        }
        stop { transition: { from: Draining, to: Stopped } }
    }
}

fn lock(machine: &Mutex<DynamicMcpActor<()>>) -> MutexGuard<'_, DynamicMcpActor<()>> {
    machine.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) struct RegistryLifecycle {
    machine: Mutex<DynamicMcpActor<()>>,
    stopped: Notify,
}

impl RegistryLifecycle {
    pub fn new() -> Self {
        Self {
            machine: Mutex::new(McpActor::new(()).into_dynamic()),
            stopped: Notify::new(),
        }
    }

    pub fn state(&self) -> McpActorState {
        lock(&self.machine).current_state()
    }

    pub fn ensure_running(&self) -> anyhow::Result<()> {
        match self.state() {
            McpActorState::Running => Ok(()),
            McpActorState::Draining => anyhow::bail!("MCP registry is shutting down"),
            McpActorState::Stopped => anyhow::bail!("MCP registry is stopped"),
        }
    }

    pub fn begin_shutdown(&self) -> McpActorState {
        let mut machine = lock(&self.machine);
        let state = machine.current_state();
        if state == McpActorState::Running {
            assert!(machine.handle(McpActorEvent::Drain).is_ok());
        }
        state
    }

    pub fn drain(&self) {
        let mut machine = lock(&self.machine);
        assert!(machine.handle(McpActorEvent::Drain).is_ok());
    }

    pub fn stop(&self) {
        assert!(lock(&self.machine).handle(McpActorEvent::Stop).is_ok());
        self.stopped.notify_waiters();
    }

    pub async fn wait_until_stopped(&self) {
        loop {
            let stopped = self.stopped.notified();
            tokio::pin!(stopped);
            stopped.as_mut().enable();
            if self.state() == McpActorState::Stopped {
                return;
            }
            stopped.await;
        }
    }
}

pub(super) struct ActorLifecycle {
    machine: Mutex<DynamicMcpActor<()>>,
    shutdown_lock: tokio::sync::Mutex<()>,
}

impl ActorLifecycle {
    pub fn new(task: JoinHandle<()>) -> Self {
        Self {
            machine: Mutex::new(
                McpActor::new(())
                    .with_owned_data(Arc::new(ActorTask(tokio::sync::Mutex::new(task))))
                    .into_dynamic(),
            ),
            shutdown_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn accepting(&self) -> bool {
        lock(&self.machine).current_state() == McpActorState::Running
    }

    fn begin_shutdown(&self) -> Option<Arc<ActorTask>> {
        let mut machine = lock(&self.machine);
        machine.handle(McpActorEvent::Drain).ok()?;
        machine.owned_data().cloned()
    }

    fn finish(&self) {
        assert!(lock(&self.machine).handle(McpActorEvent::Stop).is_ok());
    }

    pub async fn shutdown(
        &self,
        actor: &str,
        drain_timeout: Duration,
        graceful: impl Future<Output = anyhow::Result<()>>,
    ) -> anyhow::Result<()> {
        let _shutdown_guard = self.shutdown_lock.lock().await;
        let Some(owned_task) = self.begin_shutdown() else {
            return Ok(());
        };
        let graceful = tokio::time::timeout(drain_timeout, graceful).await;
        let mut task = owned_task.0.lock().await;
        let result = match graceful {
            Ok(Ok(())) => {
                match tokio::time::timeout(super::MCP_SERVER_TASK_JOIN_TIMEOUT, &mut *task).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => {
                        Err(anyhow::anyhow!("{actor} failed during shutdown: {error}"))
                    }
                    Err(_) => {
                        warn!(actor, timeout = ?super::MCP_SERVER_TASK_JOIN_TIMEOUT,
                            "MCP actor acknowledged shutdown but did not exit; aborting it");
                        task.abort();
                        let _ = (&mut *task).await;
                        Ok(())
                    }
                }
            }
            failed => {
                match failed {
                    Ok(Err(error)) => {
                        warn!(actor, %error, "MCP actor could not drain; aborting it")
                    }
                    Err(error) => warn!(actor, timeout = ?drain_timeout, %error,
                        "MCP actor did not drain; aborting it"),
                    Ok(Ok(())) => unreachable!(),
                }
                task.abort();
                let _ = (&mut *task).await;
                Ok(())
            }
        };
        self.finish();
        result
    }
}
