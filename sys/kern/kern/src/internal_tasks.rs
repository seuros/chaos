use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::anyhow;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::AgentStatus;
use chaos_mcp_runtime::ListTasksResult;
use chaos_mcp_runtime::McpTask;
use mcp_host::protocol::types::TaskStatus;
use serde_json::Value;
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::Mutex;
use tokio::time::sleep;
use uuid::Uuid;

use crate::chaos::Session;
use crate::minions::status::is_final as is_final_agent_status;
use crate::tools::context::ExecCommandToolOutput;
use crate::truncate::approx_token_count;
use crate::unified_exec::ExecTaskSnapshot;

pub(crate) const INTERNAL_TASK_SERVER_NAME: &str = "chaos_local";
const DEFAULT_POLL_INTERVAL_MS: u64 = 250;

#[derive(Debug, Clone)]
pub(crate) enum InternalTaskHandle {
    Agent { agent_id: ProcessId },
    Exec { process_id: i32 },
}

#[derive(Debug, Clone)]
struct InternalTaskRecord {
    task: McpTask,
    handle: Option<InternalTaskHandle>,
    result: Option<Value>,
}

#[derive(Default)]
pub(crate) struct InternalTaskStore {
    tasks: Mutex<HashMap<String, InternalTaskRecord>>,
}

impl InternalTaskStore {
    pub(crate) async fn create_task(
        &self,
        handle: Option<InternalTaskHandle>,
        status: TaskStatus,
        status_message: Option<String>,
        result: Option<Value>,
    ) -> McpTask {
        let now = now_timestamp();
        let task = McpTask {
            task_id: Uuid::new_v4().to_string(),
            status,
            status_message,
            created_at: now.clone(),
            last_updated_at: now,
            ttl: None,
            poll_interval: (!task_status_is_final(status)).then_some(DEFAULT_POLL_INTERVAL_MS),
        };
        self.tasks.lock().await.insert(
            task.task_id.clone(),
            InternalTaskRecord {
                task: task.clone(),
                handle,
                result,
            },
        );
        task
    }

    pub(crate) async fn list_tasks(&self) -> ListTasksResult {
        let mut tasks = self
            .tasks
            .lock()
            .await
            .values()
            .map(|record| record.task.clone())
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
        self.tasks
            .lock()
            .await
            .get(task_id)
            .map(|record| record.task.clone())
    }

    pub(crate) async fn get_task_result(&self, task_id: &str) -> Option<Value> {
        self.tasks
            .lock()
            .await
            .get(task_id)
            .and_then(|record| record.result.clone())
    }

    pub(crate) async fn get_task_handle(&self, task_id: &str) -> Option<InternalTaskHandle> {
        self.tasks
            .lock()
            .await
            .get(task_id)
            .and_then(|record| record.handle.clone())
    }

    pub(crate) async fn update_task(
        &self,
        task_id: &str,
        status: TaskStatus,
        status_message: Option<String>,
        result: Option<Value>,
        clear_handle: bool,
    ) -> Option<McpTask> {
        let mut tasks = self.tasks.lock().await;
        let record = tasks.get_mut(task_id)?;
        if task_status_is_final(record.task.status) {
            return Some(record.task.clone());
        }
        record.task.status = status;
        record.task.status_message = status_message;
        record.task.last_updated_at = now_timestamp();
        record.task.poll_interval =
            (!task_status_is_final(status)).then_some(DEFAULT_POLL_INTERVAL_MS);
        if let Some(result) = result {
            record.result = Some(result);
        }
        if clear_handle {
            record.handle = None;
        }
        Some(record.task.clone())
    }
}

pub(crate) async fn register_agent_task(
    session: Arc<Session>,
    agent_id: ProcessId,
    nickname: Option<String>,
    initial_status: AgentStatus,
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
    let task_id = task.task_id.clone();

    tokio::spawn(async move {
        let mut status_rx = match session
            .services
            .agent_control
            .subscribe_status(agent_id)
            .await
        {
            Ok(rx) => rx,
            Err(err) => {
                let _ = session
                    .services
                    .internal_task_store
                    .update_task(
                        &task_id,
                        TaskStatus::Failed,
                        Some(format!("failed to watch agent task: {err}")),
                        Some(json!({
                            "agent_id": agent_id.to_string(),
                            "nickname": nickname,
                            "error": err.to_string(),
                        })),
                        true,
                    )
                    .await;
                return;
            }
        };

        loop {
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

            if status_rx.changed().await.is_err() {
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

    task
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
        )
        .await;

    output.task_id = Some(task.task_id.clone());
    output.task_server = Some(INTERNAL_TASK_SERVER_NAME.to_string());

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

    tokio::spawn(async move {
        loop {
            match session
                .services
                .unified_exec_manager
                .task_snapshot(process_id)
                .await
            {
                Ok(ExecTaskSnapshot::Running) => {
                    sleep(Duration::from_millis(DEFAULT_POLL_INTERVAL_MS)).await;
                }
                Ok(ExecTaskSnapshot::Exited {
                    exit_code,
                    command,
                    output,
                    wall_time,
                }) => {
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
                Err(err) => {
                    let _ = session
                        .services
                        .internal_task_store
                        .update_task(
                            &task_id,
                            TaskStatus::Failed,
                            Some(format!("failed to observe exec task: {err}")),
                            Some(json!({ "error": err.to_string() })),
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
