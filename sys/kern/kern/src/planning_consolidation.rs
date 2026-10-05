use std::sync::Arc;

use anyhow::Context;
use chaos_ipc::ProcessId;
use chaos_ipc::background_tasks::{BackgroundTask, TaskSource, TaskState};
use chaos_ipc::models::{BaseInstructions, ContentItem, ResponseInputItem, ResponseItem};
use chaos_ipc::protocol::SessionSource;
use chaos_proc::planning::{
    ConsolidationInput, ConsolidationJob, ConsolidationRequest, ConsolidationState, PlanningActor,
};
use futures::StreamExt;
use serde::Serialize;

use crate::background_tasks::TaskRegistry;
use crate::chaos::{Session, TurnContext};
use crate::client::ModelClient;
use crate::client_common::{Prompt, ResponseEvent};
use crate::config::Config;
use crate::models_manager::RefreshStrategy;

fn state(job: &ConsolidationJob) -> TaskState {
    match job.state {
        ConsolidationState::Queued | ConsolidationState::Running => TaskState::Running,
        ConsolidationState::Completed => TaskState::Succeeded,
        ConsolidationState::Cancelled => TaskState::Cancelled,
        ConsolidationState::Failed | ConsolidationState::Stale => TaskState::Failed,
    }
}

fn prompt(input: &ConsolidationInput) -> anyhow::Result<Prompt> {
    #[derive(Serialize)]
    struct Rewrite<'a> {
        title: &'a str,
        body: &'a str,
        clarifications: Vec<&'a str>,
    }
    let text = serde_json::to_string(&Rewrite {
        title: &input.title,
        body: &input.body,
        clarifications: input
            .clarifications
            .iter()
            .map(|c| c.text.as_str())
            .collect(),
    })?;
    Ok(Prompt {
        input: vec![
            ResponseInputItem::Message {
                role: "user".into(),
                content: vec![ContentItem::InputText { text }],
            }
            .into(),
        ],
        base_instructions: BaseInstructions {
            text: include_str!("../templates/planning_consolidation.md").into(),
        },
        ..Prompt::default()
    })
}

impl Session {
    async fn consolidation_config(
        &self,
        config: &Config,
        request: &ConsolidationRequest,
    ) -> anyhow::Result<Config> {
        let mut config = config.clone();
        self.services
            .models_manager
            .apply_provider_binding(&mut config, &request.provider, Some(&request.model), None)
            .await?;
        Ok(config)
    }

    pub(crate) async fn request_consolidation(
        self: &Arc<Self>,
        turn: Arc<TurnContext>,
        actor: &PlanningActor,
        request: &ConsolidationRequest,
        attached: Option<&str>,
        call_id: &str,
    ) -> anyhow::Result<ConsolidationJob> {
        self.consolidation_config(&turn.config, request).await?;
        let db = self.runtime_db().context("planning database unavailable")?;
        self.begin_background_submission(call_id).await?;
        let result = db
            .planning_enqueue_consolidation(actor, request, attached, call_id)
            .await;
        let job = match result {
            Ok(job) => job,
            Err(error) => {
                self.services
                    .internal_task_store
                    .complete(
                        &TaskRegistry::submission_id(call_id),
                        TaskState::Failed,
                        Some(error.to_string()),
                        None,
                    )
                    .await;
                self.checkpoint_background_tasks().await?;
                return Err(error);
            }
        };
        self.register_consolidation(&job, false).await?;
        if job.origin_call_id != call_id {
            self.services
                .internal_task_store
                .complete(
                    &TaskRegistry::submission_id(call_id),
                    TaskState::Succeeded,
                    Some("existing consolidation request".into()),
                    Some(serde_json::to_value(&job)?),
                )
                .await;
        }
        self.start_consolidation(turn, job.clone(), false).await?;
        Ok(job)
    }

    async fn register_consolidation(
        &self,
        job: &ConsolidationJob,
        recovered: bool,
    ) -> anyhow::Result<()> {
        let id = TaskRegistry::submission_id(&job.origin_call_id);
        let registry = &self.services.internal_task_store;
        if registry.get(&id).await.is_none_or(|t| t.source.is_none()) {
            let now = jiff::Timestamp::now().to_string();
            registry
                .register(BackgroundTask {
                    id,
                    source: Some(TaskSource::PlanningConsolidation {
                        job_id: job.id.clone(),
                    }),
                    state: state(job),
                    status_message: job.error.clone(),
                    created_at: now.clone(),
                    updated_at: now,
                    result: job
                        .state
                        .is_terminal()
                        .then(|| serde_json::to_value(job))
                        .transpose()?,
                    origin_call_id: Some(job.origin_call_id.clone()),
                    origin_turn_id: None,
                    execution_id: job.execution_id.clone(),
                    ready: recovered,
                    notify: true,
                    delivered: false,
                })
                .await;
        }
        self.checkpoint_background_tasks().await
    }

    pub(crate) async fn report_consolidation(
        &self,
        task_id: &str,
        job: &ConsolidationJob,
    ) -> anyhow::Result<()> {
        self.services
            .internal_task_store
            .complete(
                task_id,
                state(job),
                job.error.clone(),
                Some(serde_json::to_value(job)?),
            )
            .await;
        self.checkpoint_background_tasks().await
    }

    pub(crate) async fn recover_consolidations(self: &Arc<Self>) -> anyhow::Result<()> {
        let Some(db) = self.runtime_db() else {
            return Ok(());
        };
        let turn = self.new_default_turn().await;
        let registry = &self.services.internal_task_store;
        // Completed database writes may precede their journal checkpoint.
        for task in registry.list().await {
            if let Some(TaskSource::PlanningConsolidation { job_id }) = &task.source {
                let job = db.planning_consolidation(job_id).await?;
                if job.state.is_terminal() {
                    self.report_consolidation(&task.id, &job).await?;
                }
            }
        }
        let mut after = String::new();
        loop {
            let jobs = db
                .planning_pending_consolidations(&self.conversation_id.to_string(), &after)
                .await?;
            if jobs.is_empty() {
                break;
            }
            for job in jobs {
                after.clone_from(&job.id);
                self.register_consolidation(&job, true).await?;
                self.start_consolidation(turn.clone(), job, true).await?;
            }
        }
        Ok(())
    }

    async fn start_consolidation(
        self: &Arc<Self>,
        turn: Arc<TurnContext>,
        job: ConsolidationJob,
        recovered: bool,
    ) -> anyhow::Result<()> {
        if job.state.is_terminal() {
            return Ok(());
        }
        let db = self.runtime_db().context("planning database unavailable")?;
        let previous = if recovered {
            job.execution_id.as_deref()
        } else {
            None
        };
        let Some(execution) = db
            .planning_claim_consolidation(&job.id, &self.conversation_id.to_string(), previous)
            .await?
        else {
            return Ok(());
        };
        let task_id = TaskRegistry::submission_id(&job.origin_call_id);
        let session = Arc::clone(self);
        let stop = self.services.internal_task_store.observer_cancel.clone();
        tokio::spawn(async move {
            let run = session.rewrite(&turn, &job);
            tokio::pin!(run);
            let finished = 'completion: {
                let result = loop {
                    tokio::select! {
                        biased;
                        _ = stop.cancelled() => return,
                        result = &mut run => break result,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                            match db.planning_consolidation(&job.id).await {
                                Ok(current) if current.state.is_terminal() => break 'completion Ok(current),
                                Ok(current) if current.execution_id.as_deref() != Some(&execution) => return,
                                Ok(_) => {}
                                Err(error) => break Err(error),
                            }
                        }
                    }
                };
                match result {
                    Ok(body) => {
                        db.planning_publish_consolidation(&job.id, &execution, &body)
                            .await
                    }
                    Err(error) => {
                        db.planning_fail_consolidation(&job.id, &execution, &error.to_string())
                            .await
                    }
                }
            };
            match finished {
                Ok(job) if job.state.is_terminal() => {
                    if let Err(error) = session.report_consolidation(&task_id, &job).await {
                        tracing::warn!(%error, "consolidation completion checkpoint failed");
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    session
                        .services
                        .internal_task_store
                        .set_blocked(Some(format!("consolidation publication pending: {error}")))
                        .await;
                }
            }
        });
        Ok(())
    }

    async fn rewrite(&self, turn: &TurnContext, job: &ConsolidationJob) -> anyhow::Result<String> {
        let config = self
            .consolidation_config(&turn.config, &job.request)
            .await?;
        let manager = &self.services.models_manager;
        let rebound;
        let manager = if manager.is_bound_to(&config.model_provider_id, &config.model_provider) {
            manager.as_ref()
        } else {
            rebound = manager
                .rebound_to(&config.model_provider_id, config.model_provider.clone())
                .context("selected model catalogue is unavailable")?;
            &rebound
        };
        manager.list_models(RefreshStrategy::Offline).await;
        let mut info = manager.get_model_info(&job.request.model, &config).await;
        info.native_server_side_tools.clear();
        let client = ModelClient::new(
            Some(
                self.services
                    .auth_manager
                    .for_provider(&job.request.provider),
            ),
            ProcessId::from_string(&job.id)?,
            job.request.provider.clone(),
            config.model_provider.clone(),
            SessionSource::Api,
            config.permissions.approval_policy.value(),
            config.model_verbosity,
            true,
            None,
            false,
            config.clamp_settings(),
        );
        let telemetry = turn
            .session_telemetry
            .clone()
            .with_model(&job.request.model, &info.slug);
        let prompt = prompt(&job.input)?;
        let mut client = client.new_session();
        let mut stream = client
            .stream(
                &prompt,
                &info,
                &telemetry,
                config.model_reasoning_effort,
                turn.reasoning_summary,
                config.service_tier,
                None,
            )
            .await?;
        let mut body = String::new();
        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. })
                    if role == "assistant" =>
                {
                    for item in content {
                        if let ContentItem::OutputText { text } = item {
                            body.push_str(&text);
                        }
                    }
                }
                ResponseEvent::Completed { .. } => {
                    anyhow::ensure!(
                        !body.trim().is_empty(),
                        "consolidation returned an empty body"
                    );
                    return Ok(body);
                }
                _ => {}
            }
        }
        anyhow::bail!("consolidation stream ended before completion")
    }
}

#[cfg(test)]
mod tests;
