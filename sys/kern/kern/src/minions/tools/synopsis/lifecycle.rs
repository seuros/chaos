use super::{
    AgentResource, AgentStatus, NormalizedJob, SynopsisJobResult, SynopsisJobState as ResultState,
};
use state_machines::state_machine;

#[derive(Debug)]
pub(super) struct JobData {
    pub id: String,
    pub agent_id: Option<String>,
    pub nickname: Option<String>,
    pub agent_type: Option<String>,
    pub status: Option<AgentStatus>,
    pub error: Option<String>,
    pub agent: Option<std::sync::Arc<AgentResource>>,
}

state_machine! {
    name: SynopsisJob,
    dynamic: true,
    initial: Pending,
    states: [
        superstate Recorded(JobData) {
            state Pending, state Running, state Completed, state Failed, state Cancelled
        }
    ],
    final_states: [Completed, Failed, Cancelled],
    events {
        start { transition: { from: Pending, to: Running } }
        complete { transition: { from: Running, to: Completed } }
        fail {
            transition: { from: Pending, to: Failed }
            transition: { from: Running, to: Failed }
        }
        cancel {
            transition: { from: Pending, to: Cancelled }
            transition: { from: Running, to: Cancelled }
        }
    }
}

pub(super) struct Job {
    machine: DynamicSynopsisJob<()>,
}

impl Job {
    pub(super) fn new(job: &NormalizedJob) -> Self {
        Self {
            machine: SynopsisJob::new(())
                .with_recorded_data(JobData {
                    id: job.id.clone(),
                    agent_id: None,
                    nickname: None,
                    agent_type: job.agent_type.clone(),
                    status: None,
                    error: None,
                    agent: None,
                })
                .into_dynamic(),
        }
    }

    pub(super) fn apply(&mut self, event: SynopsisJobEvent) -> bool {
        self.machine.handle(event).is_ok()
    }

    pub(super) fn data_mut(&mut self) -> &mut JobData {
        self.machine
            .recorded_data_mut()
            .unwrap_or_else(|| unreachable!("synopsis job owns its record"))
    }

    pub(super) fn snapshot(&self) -> SynopsisJobResult {
        let data = self
            .machine
            .recorded_data()
            .unwrap_or_else(|| unreachable!("synopsis job owns its record"));
        let state = match self.machine.current_state() {
            SynopsisJobState::Pending => ResultState::Pending,
            SynopsisJobState::Running => ResultState::Running,
            SynopsisJobState::Completed => ResultState::Completed,
            SynopsisJobState::Failed => ResultState::Failed,
            SynopsisJobState::Cancelled => ResultState::Cancelled,
        };
        SynopsisJobResult {
            id: data.id.clone(),
            state,
            agent_id: data.agent_id.clone(),
            nickname: data.nickname.clone(),
            agent_type: data.agent_type.clone(),
            status: data.status.clone(),
            error: data.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
