use super::{
    delivery::{self, TaskDelivery, TaskDeliveryEvent},
    lifecycle::{DynamicTaskExecution, TaskExecutionEvent, TaskExecutionState},
};
use chaos_ipc::background_tasks::{BackgroundTask, TaskSource, TaskState};
use serde_json::Value;
use state_machines::runtime::Parallel;

#[derive(Debug, Default)]
pub(super) struct TaskData {
    pub id: String,
    pub source: Option<TaskSource>,
    pub status_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub result: Option<Value>,
    pub origin_call_id: Option<String>,
    pub origin_turn_id: Option<String>,
    pub execution_id: Option<String>,
    pub notify: bool,
}

impl From<BackgroundTask> for TaskData {
    fn from(task: BackgroundTask) -> Self {
        Self {
            id: task.id,
            source: task.source,
            status_message: task.status_message,
            created_at: task.created_at,
            updated_at: task.updated_at,
            result: task.result,
            origin_call_id: task.origin_call_id,
            origin_turn_id: task.origin_turn_id,
            execution_id: task.execution_id,
            notify: task.notify,
        }
    }
}

pub(super) struct TaskRecord {
    regions: Parallel<DynamicTaskExecution<()>, TaskDelivery>,
}

impl TaskRecord {
    pub fn restore(task: BackgroundTask) -> Self {
        let state = match task.state {
            TaskState::Submitting => TaskExecutionState::Submitting,
            TaskState::Running => TaskExecutionState::Running,
            TaskState::InputRequired => TaskExecutionState::InputRequired,
            TaskState::Succeeded => TaskExecutionState::Succeeded,
            TaskState::Failed => TaskExecutionState::Failed,
            TaskState::Cancelled => TaskExecutionState::Cancelled,
            TaskState::Lost => TaskExecutionState::Lost,
            TaskState::SubmissionUnknown => TaskExecutionState::SubmissionUnknown,
        };
        let delivery = delivery::restore(task.ready, task.delivered);
        let mut execution = DynamicTaskExecution::new_init_state((), state);
        assert!(
            execution.set_record_data(task.into()).is_ok(),
            "task owns record data"
        );
        Self {
            regions: Parallel::new(execution, delivery),
        }
    }

    pub fn data(&self) -> &TaskData {
        self.regions
            .left()
            .record_data()
            .unwrap_or_else(|| unreachable!("task owns record data"))
    }

    pub fn data_mut(&mut self) -> &mut TaskData {
        self.regions
            .left_mut()
            .record_data_mut()
            .unwrap_or_else(|| unreachable!("task owns record data"))
    }

    pub fn state(&self) -> TaskState {
        match self.regions.left().current_state() {
            TaskExecutionState::Submitting => TaskState::Submitting,
            TaskExecutionState::Running => TaskState::Running,
            TaskExecutionState::InputRequired => TaskState::InputRequired,
            TaskExecutionState::Succeeded => TaskState::Succeeded,
            TaskExecutionState::Failed => TaskState::Failed,
            TaskExecutionState::Cancelled => TaskState::Cancelled,
            TaskExecutionState::Lost => TaskState::Lost,
            TaskExecutionState::SubmissionUnknown => TaskState::SubmissionUnknown,
        }
    }

    pub fn ready(&self) -> bool {
        delivery::ready(self.regions.right())
    }
    pub fn delivered(&self) -> bool {
        delivery::delivered(self.regions.right())
    }

    pub fn pending(&self) -> bool {
        self.state().is_terminal() && self.data().notify && self.ready() && !self.delivered()
    }

    pub fn snapshot(&self) -> BackgroundTask {
        let data = self.data();
        BackgroundTask {
            id: data.id.clone(),
            source: data.source.clone(),
            state: self.state(),
            status_message: data.status_message.clone(),
            created_at: data.created_at.clone(),
            updated_at: data.updated_at.clone(),
            result: data.result.clone(),
            origin_call_id: data.origin_call_id.clone(),
            origin_turn_id: data.origin_turn_id.clone(),
            execution_id: data.execution_id.clone(),
            ready: self.ready(),
            notify: data.notify,
            delivered: self.delivered(),
        }
    }

    pub fn transition(&mut self, next: TaskState) -> bool {
        if self.state() == next {
            return true;
        }
        let recovery = self.data().source == Some(TaskSource::MachineRecovery);
        let event = match next {
            TaskState::Running if self.state() == TaskState::Succeeded => {
                TaskExecutionEvent::WithdrawRecovery(recovery && !self.delivered())
            }
            TaskState::Cancelled if self.state() == TaskState::Succeeded => {
                TaskExecutionEvent::RevokeRecovery(recovery)
            }
            TaskState::Running => TaskExecutionEvent::Run,
            TaskState::InputRequired => TaskExecutionEvent::Input,
            TaskState::Succeeded => TaskExecutionEvent::Succeed,
            TaskState::Failed => TaskExecutionEvent::Fail,
            TaskState::Cancelled => TaskExecutionEvent::Cancel,
            TaskState::Lost => TaskExecutionEvent::Lost,
            TaskState::SubmissionUnknown => TaskExecutionEvent::Unknown,
            TaskState::Submitting => return false,
        };
        self.regions.left_mut().handle(event).is_ok()
    }

    pub fn replace(&mut self, mut task: BackgroundTask) -> bool {
        if !self.transition(task.state) {
            return false;
        }
        task.origin_turn_id = task
            .origin_turn_id
            .or_else(|| self.data().origin_turn_id.clone());
        task.execution_id = task
            .execution_id
            .or_else(|| self.data().execution_id.clone());
        task.created_at = self.data().created_at.clone();
        if task.origin_call_id != self.data().origin_call_id {
            self.delivery(TaskDeliveryEvent::BindOrigin);
        }
        if task.ready {
            self.delivery(TaskDeliveryEvent::CommitOrigin);
        }
        if task.delivered {
            self.delivery(TaskDeliveryEvent::Deliver);
        }
        *self.data_mut() = task.into();
        true
    }

    pub fn delivery(&mut self, event: TaskDeliveryEvent) {
        delivery::apply(self.regions.right_mut(), event);
    }
}
