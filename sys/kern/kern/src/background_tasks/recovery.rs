//! Recover observations, never replay execution.

use crate::chaos::Session;
use crate::rollout::RolloutRecorder;
use chaos_ipc::background_tasks::{TaskSource, TaskState};
use chaos_ipc::protocol::{EventMsg, RolloutItem};
use std::sync::Arc;

impl Session {
    pub(crate) async fn recover_background_tasks(self: &Arc<Self>, items: &[RolloutItem]) {
        let registry = &self.services.internal_task_store;
        registry.restore(items).await;
        for task in registry.list().await {
            if task.state.is_terminal() {
                continue;
            }
            match &task.source {
                Some(
                    source @ TaskSource::Mcp {
                        server,
                        remote_task_id,
                        ..
                    },
                ) => {
                    if self.mcp_task_source(server, remote_task_id).as_ref() != Some(source) {
                        registry
                            .set_blocked(Some(
                                "saved MCP endpoint is not currently configured".into(),
                            ))
                            .await;
                        continue;
                    }
                    let mut initial = crate::internal_tasks::mcp_task(&task);
                    initial.task_id = remote_task_id.clone();
                    self.observe_mcp_task(source.clone(), initial);
                }
                Some(TaskSource::Agent { process_id }) => {
                    let completed =
                        match RolloutRecorder::get_rollout_history_for_process(*process_id).await {
                            Ok(history) => history
                                .get_rollout_items()
                                .into_iter()
                                .rev()
                                .find_map(|item| match item {
                                    RolloutItem::EventMsg(EventMsg::TurnComplete(event))
                                        if task.execution_id.as_deref() == Some(&event.turn_id) =>
                                    {
                                        Some(Some(event.last_agent_message))
                                    }
                                    RolloutItem::EventMsg(EventMsg::TurnAborted(event))
                                        if task.execution_id.is_some()
                                            && task.execution_id == event.turn_id =>
                                    {
                                        Some(None)
                                    }
                                    _ => None,
                                })
                                .flatten(),
                            Err(_) => None,
                        };
                    match completed {
                        Some(message) => {
                            registry.complete(&task.id, TaskState::Succeeded, None,
                                Some(serde_json::json!({"agent_id": process_id, "message": message}))).await;
                        }
                        None => {
                            registry.complete(&task.id, TaskState::Lost,
                                Some("child execution did not survive its owner; it was not restarted".into()), None).await;
                        }
                    }
                }
                Some(TaskSource::Exec { .. }) => {
                    registry
                        .complete(
                            &task.id,
                            TaskState::Lost,
                            Some(
                                "local process did not survive its owner; command was not replayed"
                                    .into(),
                            ),
                            None,
                        )
                        .await;
                }
                Some(TaskSource::AgentMessage { .. } | TaskSource::FleetInbox { .. }) => {}
                None => {
                    registry.complete(&task.id, TaskState::SubmissionUnknown,
                        Some("submission has no durable execution handle; do not resubmit automatically".into()), None).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
