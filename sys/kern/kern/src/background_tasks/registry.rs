use std::collections::BTreeMap;

use chaos_ipc::background_tasks::{
    BackgroundTask, ProcessActivity, TaskJournalEvent, TaskSource, TaskState, WakePolicy,
};
use chaos_ipc::protocol::RolloutItem;
use serde_json::Value;
use tokio::sync::{Mutex, Notify, watch};

#[derive(Default)]
struct RegistryState {
    tasks: BTreeMap<String, BackgroundTask>,
    policy: WakePolicy,
    journal: Vec<TaskJournalEvent>,
    active_turn: bool,
    blocked: Option<String>,
    result_reads: BTreeMap<String, Vec<String>>,
    continuation: Option<String>,
    output_schema: Option<Value>,
    recovery_context: Option<chaos_ipc::background_tasks::TaskRecoveryContext>,
}

pub(crate) struct TaskRegistry {
    state: Mutex<RegistryState>,
    pub(crate) changed: Notify,
    activity: watch::Sender<ProcessActivity>,
    pub(crate) observer_cancel: tokio_util::sync::CancellationToken,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(RegistryState::default()),
            changed: Notify::new(),
            activity: watch::channel(ProcessActivity::default()).0,
            observer_cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
}

impl TaskRegistry {
    pub(crate) fn submission_id(call_id: &str) -> String {
        format!("submission:{call_id}")
    }

    pub(crate) async fn begin_submission(&self, call_id: &str) {
        let now = jiff::Timestamp::now().to_string();
        self.register(BackgroundTask {
            id: Self::submission_id(call_id),
            source: None,
            state: TaskState::Submitting,
            status_message: None,
            created_at: now.clone(),
            updated_at: now,
            result: None,
            origin_call_id: Some(call_id.into()),
            ready: false,
            notify: true,
            delivered: false,
            origin_turn_id: None,
            execution_id: None,
        })
        .await;
    }
    fn publish(&self, state: &RegistryState) {
        self.activity.send_replace(ProcessActivity {
            active_turn: state.active_turn,
            outstanding_tasks: state
                .tasks
                .values()
                .filter(|t| !t.state.is_terminal())
                .count(),
            pending_completions: state.tasks.values().filter(|t| is_pending(t)).count(),
            wake_policy: state.policy,
            blocked: state.blocked.clone().or_else(|| {
                state
                    .tasks
                    .values()
                    .any(|task| task.state == TaskState::InputRequired)
                    .then(|| "background task requires input".into())
            }),
        });
        self.changed.notify_one();
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<ProcessActivity> {
        self.activity.subscribe()
    }

    pub(crate) async fn register(&self, mut task: BackgroundTask) {
        task.result = task.result.map(bound_result);
        let mut state = self.state.lock().await;
        if let Some(previous) = state.tasks.get(&task.id) {
            task.origin_turn_id = task
                .origin_turn_id
                .or_else(|| previous.origin_turn_id.clone());
            task.execution_id = task.execution_id.or_else(|| previous.execution_id.clone());
            task.created_at = previous.created_at.clone();
        }
        state.journal.push(TaskJournalEvent::Upsert {
            task: Box::new(task.clone()),
        });
        state.tasks.insert(task.id.clone(), task);
        self.publish(&state);
    }

    pub(crate) async fn get(&self, id: &str) -> Option<BackgroundTask> {
        self.state.lock().await.tasks.get(id).cloned()
    }

    /// Atomically coalesce wake retries without reopening delivered records.
    pub(crate) async fn register_wakes_if_absent(&self, tasks: Vec<BackgroundTask>) {
        let mut state = self.state.lock().await;
        let mut changed = false;
        for task in tasks {
            if state.tasks.contains_key(&task.id) {
                continue;
            }
            state.journal.push(TaskJournalEvent::Upsert {
                task: Box::new(task.clone()),
            });
            state.tasks.insert(task.id.clone(), task);
            changed = true;
        }
        if changed {
            self.publish(&state);
        }
    }

    pub(crate) async fn list(&self) -> Vec<BackgroundTask> {
        self.state.lock().await.tasks.values().cloned().collect()
    }

    pub(crate) async fn find_source(&self, source: &TaskSource) -> Option<BackgroundTask> {
        self.state
            .lock()
            .await
            .tasks
            .values()
            .filter(|task| task.source.as_ref() == Some(source))
            .max_by_key(|task| &task.created_at)
            .cloned()
    }

    pub(crate) async fn complete(
        &self,
        id: &str,
        task_state: TaskState,
        message: Option<String>,
        result: Option<Value>,
    ) -> Option<BackgroundTask> {
        let mut state = self.state.lock().await;
        let task = state.tasks.get_mut(id)?;
        if task.state.is_terminal() {
            return Some(task.clone());
        }
        task.state = task_state;
        task.status_message = message;
        task.updated_at = jiff::Timestamp::now().to_string();
        if result.is_some() {
            task.result = result.map(bound_result);
        }
        let task = task.clone();
        state.journal.push(TaskJournalEvent::Upsert {
            task: Box::new(task.clone()),
        });
        self.publish(&state);
        Some(task)
    }

    pub(crate) async fn set_origin(&self, id: &str, call_id: &str) {
        let mut state = self.state.lock().await;
        if let Some(task) = state.tasks.get_mut(id) {
            task.origin_call_id = Some(call_id.to_owned());
            task.ready = false;
            let task = task.clone();
            state.journal.push(TaskJournalEvent::Upsert {
                task: Box::new(task),
            });
            self.publish(&state);
        }
    }

    pub(crate) async fn correlate(
        &self,
        id: &str,
        origin_turn: Option<String>,
        execution: Option<String>,
    ) {
        let mut state = self.state.lock().await;
        if let Some(task) = state.tasks.get_mut(id) {
            if origin_turn.is_some() {
                task.origin_turn_id = origin_turn;
            }
            if execution.is_some() {
                task.execution_id = execution;
            }
            let task = task.clone();
            state.journal.push(TaskJournalEvent::Upsert {
                task: Box::new(task),
            });
            self.publish(&state);
        }
    }

    /// Called by the history owner, not by a tool handler.
    pub(crate) async fn response_recorded(&self, call_id: &str) {
        let mut state = self.state.lock().await;
        let mut updates = Vec::new();
        for task in state.tasks.values_mut() {
            if task.origin_call_id.as_deref() == Some(call_id) && !task.ready {
                if task.state == TaskState::Submitting {
                    // An error response does not establish whether a remote
                    // server accepted the submission. Never retry it implicitly.
                    task.state = TaskState::SubmissionUnknown;
                    task.notify = false;
                }
                task.ready = true;
                updates.push(TaskJournalEvent::Upsert {
                    task: Box::new(task.clone()),
                });
            }
        }
        if let Some(ids) = state.result_reads.remove(call_id) {
            let mut delivered = Vec::new();
            for id in ids {
                if let Some(task) = state.tasks.get_mut(&id)
                    && !task.delivered
                {
                    task.delivered = true;
                    delivered.push(id);
                }
            }
            if !delivered.is_empty() {
                updates.push(TaskJournalEvent::Delivered {
                    task_ids: delivered,
                    turn_id: format!("tool-result:{call_id}"),
                });
            }
        }
        if !updates.is_empty() {
            state.journal.extend(updates);
            self.publish(&state);
        }
    }

    pub(crate) async fn result_read(&self, id: String, call_id: &str) {
        self.state
            .lock()
            .await
            .result_reads
            .entry(call_id.to_owned())
            .or_default()
            .push(id);
    }

    pub(crate) async fn pending(&self) -> Vec<BackgroundTask> {
        let state = self.state.lock().await;
        if state.policy != WakePolicy::Enabled
            || state.blocked.is_some()
            || state
                .tasks
                .values()
                .any(|task| task.state == TaskState::InputRequired)
        {
            return Vec::new();
        }
        state
            .tasks
            .values()
            .filter(|task| is_pending(task))
            .cloned()
            .collect()
    }

    pub(crate) async fn acknowledge(&self, ids: &[String], turn_id: &str) {
        let mut state = self.state.lock().await;
        let mut delivered = Vec::new();
        for id in ids {
            if let Some(task) = state.tasks.get_mut(id)
                && task.state.is_terminal()
                && !task.delivered
            {
                task.delivered = true;
                delivered.push(id.clone());
            }
        }
        if !delivered.is_empty() {
            state.journal.push(TaskJournalEvent::Delivered {
                task_ids: delivered,
                turn_id: turn_id.to_owned(),
            });
            self.publish(&state);
        }
    }

    pub(crate) async fn set_policy(&self, policy: WakePolicy) {
        if policy == WakePolicy::Closed {
            self.observer_cancel.cancel();
        }
        let mut state = self.state.lock().await;
        if state.policy != policy {
            state.policy = policy;
            state.journal.push(TaskJournalEvent::WakePolicy { policy });
            self.publish(&state);
        }
    }

    pub(crate) async fn set_active(&self, active: bool) {
        let mut state = self.state.lock().await;
        if state.active_turn != active {
            state.active_turn = active;
            self.publish(&state);
        }
    }

    pub(crate) async fn set_blocked(&self, reason: Option<String>) {
        let mut state = self.state.lock().await;
        if state.blocked != reason {
            state.blocked = reason;
            self.publish(&state);
        }
    }

    pub(crate) async fn take_journal(&self) -> Vec<RolloutItem> {
        std::mem::take(&mut self.state.lock().await.journal)
            .into_iter()
            .map(RolloutItem::BackgroundTask)
            .collect()
    }

    pub(crate) async fn continuation_started(&self, turn_id: &str) {
        let mut state = self.state.lock().await;
        state.continuation = Some(turn_id.into());
        state.journal.push(TaskJournalEvent::ContinuationStarted {
            turn_id: turn_id.into(),
        });
    }

    pub(crate) async fn continuation_finished(&self, turn_id: &str) {
        let mut state = self.state.lock().await;
        if state.continuation.as_deref() == Some(turn_id) {
            state.continuation = None;
            state.journal.push(TaskJournalEvent::ContinuationFinished {
                turn_id: turn_id.into(),
            });
        }
    }

    pub(crate) async fn output_schema(&self) -> Option<Value> {
        self.state.lock().await.output_schema.clone()
    }

    pub(crate) async fn set_output_schema(&self, schema: Option<Value>) {
        let mut state = self.state.lock().await;
        if state.output_schema != schema {
            state.output_schema = schema.clone();
            state
                .journal
                .push(TaskJournalEvent::OutputSchema { schema });
        }
    }

    pub(crate) async fn set_recovery_context(
        &self,
        context: chaos_ipc::background_tasks::TaskRecoveryContext,
    ) {
        let mut state = self.state.lock().await;
        if state.recovery_context.as_ref() != Some(&context) {
            state.recovery_context = Some(context.clone());
            state
                .journal
                .push(TaskJournalEvent::RecoveryContext { context });
        }
    }

    pub(crate) async fn clear_journal_block(&self) {
        let mut state = self.state.lock().await;
        if state
            .blocked
            .as_ref()
            .is_some_and(|reason| reason.starts_with("background journal unavailable:"))
        {
            state.blocked = None;
            self.publish(&state);
        }
    }

    /// Only called on resume of the same process, never on a fork or rollback.
    pub(crate) async fn restore(&self, items: &[RolloutItem]) {
        let mut state = self.state.lock().await;
        for item in items {
            let RolloutItem::BackgroundTask(event) = item else {
                continue;
            };
            match event {
                TaskJournalEvent::Upsert { task } => {
                    state.tasks.insert(task.id.clone(), task.as_ref().clone());
                }
                TaskJournalEvent::Delivered { task_ids, .. } => {
                    for id in task_ids {
                        if let Some(task) = state.tasks.get_mut(id) {
                            task.delivered = true;
                        }
                    }
                }
                TaskJournalEvent::WakePolicy { policy } => state.policy = *policy,
                TaskJournalEvent::ContinuationStarted { turn_id } => {
                    state.continuation = Some(turn_id.clone())
                }
                TaskJournalEvent::ContinuationFinished { turn_id } => {
                    if state.continuation.as_ref() == Some(turn_id) {
                        state.continuation = None;
                    }
                }
                TaskJournalEvent::OutputSchema { schema } => state.output_schema = schema.clone(),
                TaskJournalEvent::RecoveryContext { context } => {
                    state.recovery_context = Some(context.clone())
                }
            }
        }
        // Never blindly replay a model continuation which may already have
        // performed side effects before its owner crashed.
        if state.continuation.is_some() && state.policy == WakePolicy::Enabled {
            state.policy = WakePolicy::Interrupted;
        }
        // A saved handle without a committed tool response needs owner
        // reconciliation, not an out-of-order automatic notification.
        if state.tasks.values().any(|task| !task.ready) && state.policy == WakePolicy::Enabled {
            state.policy = WakePolicy::Interrupted;
        }
        self.publish(&state);
    }
}

fn is_pending(task: &BackgroundTask) -> bool {
    task.state.is_terminal() && task.ready && task.notify && !task.delivered
}

/// Keep a bounded diagnostic result, not an unbounded copy of producer output.
/// Large payloads remain explicitly marked as truncated at the resource boundary.
fn bound_result(value: Value) -> Value {
    const LIMIT: usize = 64 * 1024;
    let Ok(encoded) = serde_json::to_string(&value) else {
        return Value::Null;
    };
    if encoded.len() <= LIMIT {
        return value;
    }
    let mut end = LIMIT / 3;
    while !encoded.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::json!({"truncated": true, "preview": &encoded[..end]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completion_is_gated_deduplicated_and_resume_stable() {
        let registry = TaskRegistry::default();
        registry
            .register(BackgroundTask {
                id: "task".into(),
                source: None,
                state: TaskState::Running,
                status_message: None,
                created_at: "now".into(),
                updated_at: "now".into(),
                result: None,
                origin_call_id: Some("call".into()),
                ready: false,
                notify: true,
                delivered: false,
                origin_turn_id: None,
                execution_id: None,
            })
            .await;
        registry
            .complete("task", TaskState::Succeeded, None, Some(Value::Null))
            .await;
        assert!(registry.pending().await.is_empty());
        registry.response_recorded("call").await;
        assert_eq!(registry.pending().await.len(), 1);
        registry.set_policy(WakePolicy::Interrupted).await;
        assert!(registry.pending().await.is_empty());
        registry.set_policy(WakePolicy::Enabled).await;
        registry.acknowledge(&["task".into()], "turn").await;
        registry
            .complete("task", TaskState::Failed, None, None)
            .await;
        assert!(registry.pending().await.is_empty());
        let restored = TaskRegistry::default();
        restored.restore(&registry.take_journal().await).await;
        assert!(restored.pending().await.is_empty());
        assert_eq!(
            restored.get("task").await.unwrap().state,
            TaskState::Succeeded
        );
    }

    #[tokio::test]
    async fn explicit_result_preceding_exit_observation_is_acknowledged() {
        let registry = TaskRegistry::default();
        registry.begin_submission("exec").await;
        let id = TaskRegistry::submission_id("exec");
        registry.complete(&id, TaskState::Running, None, None).await;
        registry.response_recorded("exec").await;
        registry.result_read(id.clone(), "read").await;
        registry.response_recorded("read").await;
        registry
            .complete(&id, TaskState::Succeeded, None, Some(Value::Null))
            .await;
        assert!(registry.get(&id).await.unwrap().delivered);
        assert!(registry.pending().await.is_empty());
    }

    #[tokio::test]
    async fn interrupted_continuation_is_not_replayed_on_recovery() {
        let registry = TaskRegistry::default();
        registry.continuation_started("turn").await;
        let restored = TaskRegistry::default();
        restored.restore(&registry.take_journal().await).await;
        assert_eq!(
            restored.subscribe().borrow().wake_policy,
            WakePolicy::Interrupted
        );
    }

    #[tokio::test]
    async fn unknown_submission_is_not_reexecuted_or_notified_after_error_response() {
        let registry = TaskRegistry::default();
        registry.begin_submission("remote").await;
        registry.response_recorded("remote").await;
        assert_eq!(
            registry
                .get(&TaskRegistry::submission_id("remote"))
                .await
                .unwrap()
                .state,
            TaskState::SubmissionUnknown
        );
        assert!(registry.pending().await.is_empty());
    }

    #[test]
    fn retained_results_have_a_utf8_safe_bound() {
        let result = bound_result(Value::String("💥".repeat(100_000)));
        assert_eq!(result["truncated"], true);
        assert!(serde_json::to_vec(&result).unwrap().len() <= 64 * 1024);
    }
}
