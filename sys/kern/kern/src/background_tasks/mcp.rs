//! Adapter between the MCP runtime's observations and the kernel task registry.

use crate::chaos::Session;
use chaos_ipc::background_tasks::{BackgroundTask, TaskSource, TaskState};
use chaos_mcp_runtime::{McpTask, task_observer};
use std::sync::Arc;

impl Session {
    pub(crate) async fn owned_mcp_task(
        &self,
        server: &str,
        remote_id: &str,
    ) -> anyhow::Result<Option<BackgroundTask>> {
        if let Some(source) = self.mcp_task_source(server, remote_id)
            && let Some(task) = self.services.internal_task_store.find_source(&source).await
        {
            return Ok(Some(task));
        }
        if self
            .services
            .internal_task_store
            .list()
            .await
            .iter()
            .any(|task| {
                matches!(&task.source, Some(TaskSource::Mcp { server: owner, remote_task_id, .. })
                if owner == server && remote_task_id == remote_id)
            })
        {
            anyhow::bail!(
                "task belongs to a different MCP endpoint; refusing to reroute its handle"
            );
        }
        Ok(None)
    }
    pub(crate) fn mcp_task_source(&self, server: &str, task_id: &str) -> Option<TaskSource> {
        let configs = self.services.mcp_registry.configs_snapshot();
        let config = configs.get(server)?;
        Some(TaskSource::Mcp {
            server: server.into(),
            remote_task_id: task_id.into(),
            endpoint: task_observer::endpoint_identity(&config.transport),
        })
    }

    pub(crate) async fn track_mcp_task(
        self: &Arc<Self>,
        server: &str,
        task: McpTask,
        call_id: &str,
    ) -> anyhow::Result<()> {
        let source = self
            .mcp_task_source(server, &task.task_id)
            .ok_or_else(|| anyhow::anyhow!("MCP server disappeared during task registration"))?;
        let now = jiff::Timestamp::now().to_string();
        let record = BackgroundTask {
            id: super::TaskRegistry::submission_id(call_id),
            source: Some(source.clone()),
            state: TaskState::Running,
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
        };
        self.services.internal_task_store.register(record).await;
        self.checkpoint_background_tasks().await?;
        self.observe_mcp_task(source, task);
        Ok(())
    }

    pub(crate) fn observe_mcp_task(self: &Arc<Self>, source: TaskSource, initial: McpTask) {
        let TaskSource::Mcp {
            server,
            remote_task_id,
            ..
        } = source.clone()
        else {
            return;
        };
        let weak = Arc::downgrade(self);
        let fetch_source = source.clone();
        let mut observations = task_observer::observe_task(
            initial,
            self.services
                .internal_task_store
                .observer_cancel
                .child_token(),
            move || {
                let weak = weak.clone();
                let server = server.clone();
                let remote_task_id = remote_task_id.clone();
                let source = fetch_source.clone();
                async move {
                    let session = weak
                        .upgrade()
                        .ok_or_else(|| anyhow::anyhow!("session closed"))?;
                    if session.mcp_task_source(&server, &remote_task_id).as_ref() != Some(&source) {
                        anyhow::bail!(
                            "MCP endpoint changed; original task must be reconciled by its owner"
                        );
                    }
                    if let Some(record) = session
                        .services
                        .internal_task_store
                        .find_source(&source)
                        .await
                        && record.state.is_terminal()
                    {
                        let mut task = crate::internal_tasks::mcp_task(&record);
                        task.task_id = remote_task_id;
                        return Ok(task);
                    }
                    session.get_mcp_task(&server, &remote_task_id).await
                }
            },
        );
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(observation) = observations.recv().await {
                let Some(session) = weak.upgrade() else { break };
                match observation {
                    task_observer::TaskObservation::Status(task) => {
                        session.apply_mcp_task_observation(&source, task).await;
                    }
                    task_observer::TaskObservation::Unavailable(error) => {
                        // Observation failure is not a remote terminal outcome.
                        tracing::warn!(%error, "MCP task observation deferred");
                    }
                    task_observer::TaskObservation::Lost(error) => {
                        if let Some(task) = session
                            .services
                            .internal_task_store
                            .find_source(&source)
                            .await
                        {
                            session
                                .services
                                .internal_task_store
                                .complete(
                                    &task.id,
                                    TaskState::Lost,
                                    Some(format!("remote handle expired or is unknown: {error}")),
                                    None,
                                )
                                .await;
                        }
                    }
                }
            }
        });
    }

    pub(crate) async fn apply_mcp_task_observation(&self, source: &TaskSource, task: McpTask) {
        let registry = &self.services.internal_task_store;
        let Some(record) = registry.find_source(source).await else {
            return;
        };
        if record.state.is_terminal() {
            return;
        }
        let status = crate::internal_tasks::native_status(task.status);
        if !status.is_terminal() {
            // Do not journal identical polling snapshots.
            if record.state != status {
                registry.complete(&record.id, status, None, None).await;
            }
            return;
        }
        let TaskSource::Mcp {
            server,
            remote_task_id,
            ..
        } = source
        else {
            return;
        };
        let result = match self.get_mcp_task_result(server, remote_task_id).await {
            Ok(result) => match serde_json::to_value(result) {
                Ok(value) => Some(value),
                Err(error) => {
                    tracing::warn!(%error, "could not retain MCP task result");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(%error, "MCP task is terminal but its result is unavailable");
                None
            }
        };
        registry.complete(&record.id, status, None, result).await;
    }
}
