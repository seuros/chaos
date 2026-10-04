use crate::config::Config;
use crate::error::ChaosErr;
use crate::minions::control::AgentControl;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::user_input::UserInput;
use state_machines::state_machine;
use std::collections::HashMap;
use tokio::time::Instant;
use tracing::Instrument;

pub(super) struct Worker {
    pub item_id: String,
    pub started_at: Instant,
    process_id: Option<ProcessId>,
    control: AgentControl,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("process_id", &self.process_id)
            .finish_non_exhaustive()
    }
}

impl Worker {
    pub fn recover(
        control: AgentControl,
        process_id: ProcessId,
        item_id: String,
        started_at: Instant,
    ) -> Self {
        Self {
            item_id,
            started_at,
            process_id: Some(process_id),
            control,
        }
    }

    pub async fn spawn(
        control: AgentControl,
        config: Config,
        items: Vec<UserInput>,
        source: SessionSource,
        item_id: String,
    ) -> Result<Self, ChaosErr> {
        tokio::spawn(
            async move {
                let process_id = control.spawn_agent(config, items, Some(source)).await?;
                Ok::<Self, ChaosErr>(Self::recover(control, process_id, item_id, Instant::now()))
            }
            .in_current_span(),
        )
        .await
        .map_err(|error| {
            ChaosErr::UnsupportedOperation(format!("minion worker spawn task failed: {error}"))
        })?
    }

    pub fn process_id(&self) -> ProcessId {
        self.process_id
            .unwrap_or_else(|| unreachable!("live worker owns its process"))
    }

    async fn close(&mut self) {
        if let Some(process_id) = self.process_id {
            let _ = self.control.shutdown_agent(process_id).await;
            self.process_id = None;
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let Some(process_id) = self.process_id.take() else {
            return;
        };
        let control = self.control.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = control.shutdown_agent(process_id).await;
            });
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Workers {
    items: HashMap<ProcessId, Worker>,
}

state_machine! {
    name: CsvWorkers,
    dynamic: true,
    initial: Accepting,
    states: [
        superstate Owned(Workers) {
            state Accepting, state Draining
        },
        Completed, Cancelled
    ],
    final_states: [Completed, Cancelled],
    events {
        drain { transition: { from: Accepting, to: Draining } }
        finish {
            transition: { from: Accepting, to: Completed }
            transition: { from: Draining, to: Cancelled }
        }
    }
}

pub(super) struct WorkerPool {
    machine: DynamicCsvWorkers<()>,
}

impl WorkerPool {
    pub fn new() -> Self {
        Self {
            machine: CsvWorkers::new(())
                .with_owned_data(Workers::default())
                .into_dynamic(),
        }
    }

    pub fn items(&self) -> &HashMap<ProcessId, Worker> {
        &self
            .machine
            .owned_data()
            .unwrap_or_else(|| unreachable!("active pool owns workers"))
            .items
    }

    fn items_mut(&mut self) -> &mut HashMap<ProcessId, Worker> {
        &mut self
            .machine
            .owned_data_mut()
            .unwrap_or_else(|| unreachable!("active pool owns workers"))
            .items
    }

    pub fn adopt(&mut self, mut worker: Worker) {
        match self.items_mut().entry(worker.process_id()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(worker);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let owned = entry.get_mut();
                owned.item_id = std::mem::take(&mut worker.item_id);
                owned.started_at = worker.started_at;
                worker.process_id = None;
            }
        }
    }

    pub fn assigned(&mut self, process_id: ProcessId) {
        self.items_mut()
            .get_mut(&process_id)
            .unwrap_or_else(|| unreachable!("assigned worker was adopted"))
            .started_at = Instant::now();
    }

    pub fn cancelled(&self) -> bool {
        matches!(
            self.machine.current_state(),
            CsvWorkersState::Draining | CsvWorkersState::Cancelled
        )
    }

    pub fn drain(&mut self) {
        assert!(self.machine.handle(CsvWorkersEvent::Drain).is_ok());
    }

    pub async fn close(&mut self, process_id: ProcessId) {
        if let Some(worker) = self.items_mut().get_mut(&process_id) {
            worker.close().await;
        }
        self.items_mut().remove(&process_id);
    }

    pub fn finish(&mut self) {
        assert!(self.items().is_empty());
        assert!(self.machine.handle(CsvWorkersEvent::Finish).is_ok());
    }
}
