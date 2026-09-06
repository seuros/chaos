use std::sync::Arc;
use std::time::Duration;

pub(crate) use crate::background_tasks::TaskRegistry as InternalTaskStore;
use anyhow::Context;
use anyhow::anyhow;
use chaos_ipc::ProcessId;
use chaos_ipc::background_tasks::{BackgroundTask, TaskSource, TaskState};
use chaos_ipc::protocol::AgentStatus;
use chaos_mcp_runtime::ListTasksResult;
use chaos_mcp_runtime::McpTask;
use mcp_host::protocol::types::TaskStatus;
use serde_json::Value;
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::chaos::Session;
use crate::minions::status::is_final as is_final_agent_status;
use crate::tools::context::ExecCommandToolOutput;
use crate::truncate::approx_token_count;
use crate::unified_exec::ExecTaskSnapshot;

const DEFAULT_POLL_INTERVAL_MS: u64 = 250;

#[derive(Debug, Clone)]
pub(crate) enum InternalTaskHandle {
    Agent { agent_id: ProcessId },
    Exec { process_id: i32 },
}

impl InternalTaskStore {
    pub(crate) async fn create_task(
        &self,
        handle: Option<InternalTaskHandle>,
        status: TaskStatus,
        status_message: Option<String>,
        result: Option<Value>,
        call_id: Option<&str>,
    ) -> McpTask {
        let now = now_timestamp();
        let source = handle.map(|handle| match handle {
            InternalTaskHandle::Exec { process_id } => TaskSource::Exec {
                session_id: process_id,
            },
            InternalTaskHandle::Agent { agent_id } => TaskSource::Agent {
                process_id: agent_id,
            },
        });
        let task = BackgroundTask {
            id: call_id
                .map(Self::submission_id)
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            source: source.clone(),
            state: native_status(status),
            status_message,
            created_at: now.clone(),
            updated_at: now,
            result,
            origin_call_id: call_id.map(str::to_owned),
            origin_turn_id: None,
            execution_id: None,
            ready: call_id.is_none(),
            notify: matches!(source, Some(TaskSource::Agent { .. }))
                || !task_status_is_final(status),
            delivered: false,
        };
        self.register(task.clone()).await;
        mcp_task(&task)
    }

    pub(crate) async fn list_tasks(&self) -> ListTasksResult {
        let mut tasks = self
            .list()
            .await
            .iter()
            .filter(|task| !matches!(task.source, Some(TaskSource::Mcp { .. })))
            .map(mcp_task)
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.task_id.cmp(&b.task_id))
        });
        ListTasksResult {
            tasks,
            next_cursor: None,
            meta: None,
        }
    }

    pub(crate) async fn get_task(&self, task_id: &str) -> Option<McpTask> {
        self.get(task_id).await.as_ref().map(mcp_task)
    }

    pub(crate) async fn get_task_result(&self, task_id: &str) -> Option<Value> {
        self.get(task_id).await.and_then(|task| task.result)
    }

    pub(crate) async fn get_task_handle(&self, task_id: &str) -> Option<InternalTaskHandle> {
        let task = self.get(task_id).await?;
        if task.state.is_terminal() {
            return None;
        }
        match task.source? {
            TaskSource::Exec { session_id } => Some(InternalTaskHandle::Exec {
                process_id: session_id,
            }),
            TaskSource::Agent { process_id } => Some(InternalTaskHandle::Agent {
                agent_id: process_id,
            }),
            TaskSource::Mcp { .. }
            | TaskSource::AgentMessage { .. }
            | TaskSource::FleetInbox { .. } => None,
        }
    }

    pub(crate) async fn update_task(
        &self,
        task_id: &str,
        status: TaskStatus,
        status_message: Option<String>,
        result: Option<Value>,
        _clear_handle: bool,
    ) -> Option<McpTask> {
        self.complete(task_id, native_status(status), status_message, result)
            .await
            .as_ref()
            .map(mcp_task)
    }
}

pub(crate) fn native_status(status: TaskStatus) -> TaskState {
    match status {
        TaskStatus::Working => TaskState::Running,
        TaskStatus::InputRequired => TaskState::InputRequired,
        TaskStatus::Completed => TaskState::Succeeded,
        TaskStatus::Failed => TaskState::Failed,
        TaskStatus::Cancelled => TaskState::Cancelled,
    }
}

pub(crate) fn mcp_task(task: &BackgroundTask) -> McpTask {
    McpTask {
        task_id: task.id.clone(),
        status: match task.state {
            TaskState::Submitting | TaskState::Running => TaskStatus::Working,
            TaskState::InputRequired => TaskStatus::InputRequired,
            TaskState::Succeeded => TaskStatus::Completed,
            TaskState::Failed | TaskState::Lost | TaskState::SubmissionUnknown => {
                TaskStatus::Failed
            }
            TaskState::Cancelled => TaskStatus::Cancelled,
        },
        status_message: task.status_message.clone(),
        created_at: task.created_at.clone(),
        last_updated_at: task.updated_at.clone(),
        ttl: None,
        poll_interval: (!task.state.is_terminal()).then_some(DEFAULT_POLL_INTERVAL_MS),
    }
}

pub(crate) async fn register_agent_task(
    session: Arc<Session>,
    agent_id: ProcessId,
    nickname: Option<String>,
    initial_status: AgentStatus,
    call_id: Option<&str>,
) -> McpTask {
    let task = session
        .services
        .internal_task_store
        .create_task(
            Some(InternalTaskHandle::Agent { agent_id }),
            agent_task_status(&initial_status),
            Some(agent_status_message(&initial_status)),
            is_final_agent_status(&initial_status)
                .then(|| agent_result_value(agent_id, nickname.clone(), &initial_status)),
            call_id,
        )
        .await;

    if is_final_agent_status(&initial_status) {
        let _ = session
            .services
            .internal_task_store
            .update_task(
                &task.task_id,
                agent_task_status(&initial_status),
                Some(agent_status_message(&initial_status)),
                Some(agent_result_value(agent_id, nickname, &initial_status)),
                true,
            )
            .await;
        return task;
    }
    match session
        .services
        .agent_control
        .subscribe_status(agent_id)
        .await
    {
        Ok(rx) => watch_agent_task(
            &session,
            task.task_id.clone(),
            agent_id,
            nickname,
            rx,
            false,
        ),
        Err(error) => {
            session
                .services
                .internal_task_store
                .complete(
                    &task.task_id,
                    TaskState::Lost,
                    Some(format!("cannot observe child: {error}")),
                    None,
                )
                .await;
        }
    }
    task
}

/// Enroll another execution generation only for an already-enrolled child.
/// Internal reviewers/orchestrators which suppressed completion stay suppressed.
pub(crate) async fn prepare_agent_input(
    session: &Arc<Session>,
    agent_id: ProcessId,
    call_id: &str,
) -> anyhow::Result<()> {
    let Some(previous) = session
        .services
        .internal_task_store
        .find_source(&TaskSource::Agent {
            process_id: agent_id,
        })
        .await
    else {
        return Ok(());
    };
    let rx = session
        .services
        .agent_control
        .subscribe_status(agent_id)
        .await?;
    let status = rx.borrow().clone();
    let wait_for_change = is_final_agent_status(&status);
    if !previous.state.is_terminal() {
        if !wait_for_change {
            return Ok(());
        }
        session
            .services
            .internal_task_store
            .complete(
                &previous.id,
                native_status(agent_task_status(&status)),
                Some(agent_status_message(&status)),
                Some(agent_result_value(agent_id, None, &status)),
            )
            .await;
    }
    session.begin_background_submission(call_id).await?;
    let task = session
        .services
        .internal_task_store
        .create_task(
            Some(InternalTaskHandle::Agent { agent_id }),
            TaskStatus::Working,
            None,
            None,
            Some(call_id),
        )
        .await;
    session.checkpoint_background_tasks().await?;
    watch_agent_task(session, task.task_id, agent_id, None, rx, wait_for_change);
    Ok(())
}

fn watch_agent_task(
    session: &Arc<Session>,
    task_id: String,
    agent_id: ProcessId,
    nickname: Option<String>,
    mut status_rx: tokio::sync::watch::Receiver<AgentStatus>,
    wait_for_change: bool,
) {
    let cancel = session
        .services
        .internal_task_store
        .observer_cancel
        .child_token();
    let weak = Arc::downgrade(session);
    tokio::spawn(async move {
        if wait_for_change {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = status_rx.changed() => {}
            }
        }
        loop {
            let Some(session) = weak.upgrade() else { break };
            let status = status_rx.borrow().clone();
            if is_final_agent_status(&status) {
                let _ = session
                    .services
                    .internal_task_store
                    .update_task(
                        &task_id,
                        agent_task_status(&status),
                        Some(agent_status_message(&status)),
                        Some(agent_result_value(agent_id, nickname.clone(), &status)),
                        true,
                    )
                    .await;
                break;
            }

            drop(session);
            let changed = tokio::select! {
                _ = cancel.cancelled() => break,
                result = status_rx.changed() => result,
            };
            if changed.is_err() {
                let Some(session) = weak.upgrade() else { break };
                let latest = session.services.agent_control.get_status(agent_id).await;
                let final_status = if is_final_agent_status(&latest) {
                    latest
                } else {
                    AgentStatus::Errored("agent status stream closed unexpectedly".to_string())
                };
                let _ = session
                    .services
                    .internal_task_store
                    .update_task(
                        &task_id,
                        agent_task_status(&final_status),
                        Some(agent_status_message(&final_status)),
                        Some(agent_result_value(
                            agent_id,
                            nickname.clone(),
                            &final_status,
                        )),
                        true,
                    )
                    .await;
                break;
            }
        }
    });
}

pub(crate) async fn attach_exec_task(
    session: Arc<Session>,
    output: &mut ExecCommandToolOutput,
) -> anyhow::Result<()> {
    let task = session
        .services
        .internal_task_store
        .create_task(
            output
                .process_id
                .map(|process_id| InternalTaskHandle::Exec { process_id }),
            match output.process_id {
                Some(_) => TaskStatus::Working,
                None => exec_exit_task_status(output.exit_code),
            },
            Some(match output.process_id {
                Some(_) => "command is still running".to_string(),
                None => exec_status_message(output.exit_code),
            }),
            (output.process_id.is_none()).then(|| exec_result_from_output(output)),
            Some(&output.event_call_id),
        )
        .await;

    output.task_id = Some(task.task_id.clone());
    session
        .services
        .internal_task_store
        .set_origin(&task.task_id, &output.event_call_id)
        .await;

    let Some(process_id) = output.process_id else {
        let _ = session
            .services
            .internal_task_store
            .update_task(
                &task.task_id,
                exec_exit_task_status(output.exit_code),
                Some(exec_status_message(output.exit_code)),
                Some(exec_result_from_output(output)),
                true,
            )
            .await;
        return Ok(());
    };
    let task_id = task.task_id.clone();
    let mut completion = session
        .services
        .unified_exec_manager
        .subscribe_completion(process_id)
        .await
        .map_err(|error| anyhow!(error.to_string()))?;

    tokio::spawn(async move {
        loop {
            let snapshot = completion.borrow_and_update().clone();
            match snapshot {
                ExecTaskSnapshot::Running => {
                    if completion.changed().await.is_err() {
                        session
                            .services
                            .internal_task_store
                            .complete(
                                &task_id,
                                TaskState::Lost,
                                Some("process completion channel closed".into()),
                                None,
                            )
                            .await;
                        break;
                    }
                }
                ExecTaskSnapshot::Exited {
                    exit_code,
                    command,
                    output,
                    wall_time,
                } => {
                    let _ = session
                        .services
                        .internal_task_store
                        .update_task(
                            &task_id,
                            exec_exit_task_status(exit_code),
                            Some(exec_status_message(exit_code)),
                            Some(exec_result_from_snapshot(
                                exit_code, command, output, wall_time,
                            )),
                            true,
                        )
                        .await;
                    break;
                }
            }
        }
    });

    Ok(())
}

impl Session {
    pub(crate) async fn list_internal_tasks(&self) -> ListTasksResult {
        self.services.internal_task_store.list_tasks().await
    }

    pub(crate) async fn get_internal_task(&self, task_id: &str) -> anyhow::Result<McpTask> {
        self.services
            .internal_task_store
            .get_task(task_id)
            .await
            .ok_or_else(|| anyhow!("unknown internal task '{task_id}'"))
    }

    pub(crate) async fn get_internal_task_result(&self, task_id: &str) -> anyhow::Result<Value> {
        let task = self.get_internal_task(task_id).await?;
        if !task_status_is_final(task.status) {
            anyhow::bail!("task '{task_id}' is not finished yet");
        }
        self.services
            .internal_task_store
            .get_task_result(task_id)
            .await
            .ok_or_else(|| anyhow!("task '{task_id}' has no result"))
    }

    pub(crate) async fn cancel_internal_task(&self, task_id: &str) -> anyhow::Result<McpTask> {
        let current = self.get_internal_task(task_id).await?;
        if task_status_is_final(current.status) {
            return Ok(current);
        }

        match self
            .services
            .internal_task_store
            .get_task_handle(task_id)
            .await
            .context("task has no active handle")?
        {
            InternalTaskHandle::Agent { agent_id } => {
                let _ = self.services.agent_control.shutdown_agent(agent_id).await;
                self.services
                    .internal_task_store
                    .update_task(
                        task_id,
                        TaskStatus::Cancelled,
                        Some("agent task cancelled".to_string()),
                        Some(json!({
                            "agent_id": agent_id.to_string(),
                            "status": "cancelled",
                        })),
                        true,
                    )
                    .await
                    .ok_or_else(|| anyhow!("unknown internal task '{task_id}'"))
            }
            InternalTaskHandle::Exec { process_id } => {
                self.services
                    .unified_exec_manager
                    .terminate_process(process_id)
                    .await
                    .map_err(|err| anyhow!(err.to_string()))?;
                self.services
                    .internal_task_store
                    .update_task(
                        task_id,
                        TaskStatus::Cancelled,
                        Some("command cancelled".to_string()),
                        Some(json!({
                            "session_id": process_id,
                            "status": "cancelled",
                        })),
                        true,
                    )
                    .await
                    .ok_or_else(|| anyhow!("unknown internal task '{task_id}'"))
            }
        }
    }
}

fn agent_task_status(status: &AgentStatus) -> TaskStatus {
    match status {
        AgentStatus::Completed(_) => TaskStatus::Completed,
        AgentStatus::Errored(_) | AgentStatus::NotFound => TaskStatus::Failed,
        AgentStatus::Shutdown => TaskStatus::Cancelled,
        AgentStatus::PendingInit | AgentStatus::Running | AgentStatus::Interrupted => {
            TaskStatus::Working
        }
    }
}

fn agent_status_message(status: &AgentStatus) -> String {
    match status {
        AgentStatus::PendingInit => "agent task is starting".to_string(),
        AgentStatus::Running => "agent task is running".to_string(),
        AgentStatus::Completed(_) => "agent task completed".to_string(),
        AgentStatus::Errored(message) => format!("agent task failed: {message}"),
        AgentStatus::Interrupted => "agent task was interrupted".to_string(),
        AgentStatus::Shutdown => "agent task was shut down".to_string(),
        AgentStatus::NotFound => "agent task not found".to_string(),
    }
}

fn agent_result_value(
    agent_id: ProcessId,
    nickname: Option<String>,
    status: &AgentStatus,
) -> Value {
    json!({
        "agent_id": agent_id.to_string(),
        "nickname": nickname,
        "status": status,
    })
}

fn exec_result_from_output(output: &ExecCommandToolOutput) -> Value {
    json!({
        "chunk_id": (!output.chunk_id.is_empty()).then_some(output.chunk_id.clone()),
        "wall_time_seconds": output.wall_time.as_secs_f64(),
        "exit_code": output.exit_code,
        "session_id": output.process_id,
        "original_token_count": output.original_token_count,
        "output": output.truncated_output(),
    })
}

fn exec_result_from_snapshot(
    exit_code: Option<i32>,
    command: Vec<String>,
    output: Vec<u8>,
    wall_time: Duration,
) -> Value {
    let text = String::from_utf8_lossy(&output).to_string();
    json!({
        "command": command,
        "wall_time_seconds": wall_time.as_secs_f64(),
        "exit_code": exit_code,
        "session_id": Value::Null,
        "original_token_count": approx_token_count(&text),
        "output": text,
    })
}

fn exec_exit_task_status(exit_code: Option<i32>) -> TaskStatus {
    match exit_code {
        Some(0) => TaskStatus::Completed,
        Some(_) | None => TaskStatus::Failed,
    }
}

fn exec_status_message(exit_code: Option<i32>) -> String {
    match exit_code {
        Some(0) => "command completed".to_string(),
        Some(code) => format!("command failed with exit code {code}"),
        None => "command finished".to_string(),
    }
}

fn task_status_is_final(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

fn now_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
