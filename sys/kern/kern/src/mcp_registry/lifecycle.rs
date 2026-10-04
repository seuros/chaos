use state_machines::state_machine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
        self.machine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current_state()
            == McpActorState::Running
    }

    fn begin_shutdown(&self) -> Option<Arc<ActorTask>> {
        let mut machine = self
            .machine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        machine.handle(McpActorEvent::Drain).ok()?;
        machine.owned_data().cloned()
    }

    fn finish(&self) {
        let mut machine = self
            .machine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(machine.handle(McpActorEvent::Stop).is_ok());
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
