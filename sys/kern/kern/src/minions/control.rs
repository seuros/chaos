use crate::error::ChaosErr;
use crate::error::Result as CodexResult;
use crate::minions::AgentStatus;
use crate::minions::guards::Guards;
use crate::minions::role::DEFAULT_ROLE_NAME;
use crate::minions::role::resolve_role_config;
use crate::minions::status::is_final;
use crate::process_table::ProcessTableState;
use crate::rollout::RolloutRecorder;
use crate::session_prefix::format_subagent_context_line;
use crate::session_prefix::format_subagent_notification_message;
use crate::shell_snapshot::ShellSnapshot;
use crate::state_db;
use chaos_ipc::ProcessId;
use chaos_ipc::models::FunctionCallOutputPayload;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::user_input::UserInput;
use std::sync::Arc;
use std::sync::Weak;
use tokio::sync::watch;

const FORKED_SPAWN_AGENT_OUTPUT_MESSAGE: &str = "You are the newly spawned agent. The prior conversation history was forked from your parent agent. Treat the next user message as your new task, and use the forked history only as background context.";

#[derive(Clone, Debug, Default)]
pub(crate) struct SpawnAgentOptions {
    pub(crate) fork_parent_spawn_call_id: Option<String>,
}

fn agent_nickname_candidates(
    config: &crate::config::Config,
    role_name: Option<&str>,
) -> Vec<String> {
    let role_name = role_name.unwrap_or(DEFAULT_ROLE_NAME);
    let role_candidates =
        resolve_role_config(config, role_name).and_then(|role| role.nickname_candidates.clone());
    chaos_minions::nickname_candidates(role_candidates)
}

/// Control-plane handle for multi-agent operations.
/// `AgentControl` is held by each session (via `SessionServices`). It provides capability to
/// spawn new agents and the inter-agent communication layer.
/// An `AgentControl` instance is shared per "user session" which means the same `AgentControl`
/// is used for every sub-agent spawned by Codex. By doing so, we make sure the guards are
/// scoped to a user session.
#[derive(Clone, Default)]
pub(crate) struct AgentControl {
    /// Weak handle back to the global process registry/state.
    /// This is `Weak` to avoid reference cycles and shadow persistence of the form
    /// `ProcessTableState -> Process -> Session -> SessionServices -> ProcessTableState`.
    manager: Weak<ProcessTableState>,
    state: Arc<Guards>,
}

impl AgentControl {
    /// Construct a new `AgentControl` that can spawn/message agents via the given manager state.
    pub(crate) fn new(manager: Weak<ProcessTableState>) -> Self {
        Self {
            manager,
            ..Default::default()
        }
    }

    /// Spawn a new agent thread and submit the initial prompt.
    pub(crate) async fn spawn_agent(
        &self,
        config: crate::config::Config,
        items: Vec<UserInput>,
        session_source: Option<SessionSource>,
    ) -> CodexResult<ProcessId> {
        self.spawn_agent_with_options(config, items, session_source, SpawnAgentOptions::default())
            .await
    }

    pub(crate) async fn spawn_agent_with_options(
        &self,
        config: crate::config::Config,
        items: Vec<UserInput>,
        session_source: Option<SessionSource>,
        options: SpawnAgentOptions,
    ) -> CodexResult<ProcessId> {
        let state = self.upgrade()?;
        let mut reservation = self.state.reserve_spawn_slot(config.agent_max_threads)?;
        let inherited_shell_snapshot = self
            .inherited_shell_snapshot_for_source(&state, session_source.as_ref())
            .await;
        let session_source = match session_source {
            Some(SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                parent_process_id,
                depth,
                agent_role,
                ..
            })) => {
                let candidate_names = agent_nickname_candidates(&config, agent_role.as_deref());
                let candidate_name_refs: Vec<&str> =
                    candidate_names.iter().map(String::as_str).collect();
                let agent_nickname = reservation.reserve_agent_nickname(&candidate_name_refs)?;
                Some(SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                    parent_process_id,
                    depth,
                    agent_nickname: Some(agent_nickname),
                    agent_role,
                }))
            }
            other => other,
        };
        let notification_source = session_source.clone();

        // The same `AgentControl` is sent to spawn the process.
        let new_process = match session_source {
            Some(session_source) => {
                if let Some(call_id) = options.fork_parent_spawn_call_id.as_ref() {
                    let SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                        parent_process_id,
                        ..
                    }) = session_source.clone()
                    else {
                        return Err(ChaosErr::Fatal(
                            "spawn_agent fork requires a thread-spawn session source".to_string(),
                        ));
                    };
                    let parent_thread = state.get_process(parent_process_id).await.ok();
                    if let Some(parent_thread) = parent_thread.as_ref() {
                        // `record_conversation_items` only queues rollout writes asynchronously.
                        // Flush the live parent before snapshotting history for a fork.
                        parent_thread
                            .codex
                            .session
                            .ensure_rollout_materialized()
                            .await;
                        parent_thread.codex.session.flush_rollout().await;
                    }
                    let mut forked_rollout_items =
                        RolloutRecorder::get_rollout_history_for_process(parent_process_id)
                            .await?
                            .get_rollout_items();
                    let mut output = FunctionCallOutputPayload::from_text(
                        FORKED_SPAWN_AGENT_OUTPUT_MESSAGE.to_string(),
                    );
                    output.success = Some(true);
                    forked_rollout_items.push(RolloutItem::ResponseItem(
                        ResponseItem::FunctionCallOutput {
                            call_id: call_id.clone(),
                            output,
                        },
                    ));
                    let initial_history = InitialHistory::Forked(forked_rollout_items);
                    state
                        .fork_process_with_source(
                            config,
                            initial_history,
                            self.clone(),
                            session_source,
                            /*persist_extended_history*/ false,
                            inherited_shell_snapshot,
                        )
                        .await?
                } else {
                    state
                        .spawn_new_process_with_source(
                            config,
                            self.clone(),
                            session_source,
                            /*persist_extended_history*/ false,
                            /*metrics_service_name*/ None,
                            inherited_shell_snapshot,
                        )
                        .await?
                }
            }
            None => state.spawn_new_process(config, self.clone()).await?,
        };
        let process_id = new_process.process_id();
        reservation.commit(process_id);

        // Notify a new process has been created. This notification will be processed by clients
        // to subscribe or drain this newly created process.
        // TODO(jif) add helper for drain
        state.notify_process_created(process_id);

        self.send_input(process_id, items).await?;
        self.maybe_start_completion_watcher(process_id, notification_source);

        Ok(process_id)
    }

    /// Resume an existing agent thread from persisted journal history.
    pub(crate) async fn resume_agent_from_rollout(
        &self,
        config: crate::config::Config,
        process_id: ProcessId,
        session_source: SessionSource,
    ) -> CodexResult<ProcessId> {
        let state = self.upgrade()?;
        let mut reservation = self.state.reserve_spawn_slot(config.agent_max_threads)?;
        let session_source = match session_source {
            SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                parent_process_id,
                depth,
                ..
            }) => {
                // Collab resume callers rebuild a placeholder ProcessSpawn source. Rehydrate the
                // stored nickname/role from sqlite when available; otherwise leave both unset.
                let (resumed_agent_nickname, resumed_agent_role) =
                    if let Some(state_db_ctx) = state_db::get_state_db(&config).await {
                        match state_db_ctx.get_process(process_id).await {
                            Ok(Some(metadata)) => (metadata.agent_nickname, metadata.agent_role),
                            Ok(None) | Err(_) => (None, None),
                        }
                    } else {
                        (None, None)
                    };
                let reserved_agent_nickname = resumed_agent_nickname
                    .as_deref()
                    .map(|agent_nickname| {
                        let candidate_names =
                            agent_nickname_candidates(&config, resumed_agent_role.as_deref());
                        let candidate_name_refs: Vec<&str> =
                            candidate_names.iter().map(String::as_str).collect();
                        reservation.reserve_agent_nickname_with_preference(
                            &candidate_name_refs,
                            Some(agent_nickname),
                        )
                    })
                    .transpose()?;
                SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                    parent_process_id,
                    depth,
                    agent_nickname: reserved_agent_nickname,
                    agent_role: resumed_agent_role,
                })
            }
            other => other,
        };
        let notification_source = session_source.clone();
        let inherited_shell_snapshot = self
            .inherited_shell_snapshot_for_source(&state, Some(&session_source))
            .await;
        let resumed_process = state
            .resume_process_with_source(
                config,
                process_id,
                self.clone(),
                session_source,
                inherited_shell_snapshot,
            )
            .await?;
        let process_id = resumed_process.process_id();
        reservation.commit(process_id);
        // Resumed processes are re-registered in-memory and need the same listener
        // attachment path as freshly spawned processes.
        state.notify_process_created(process_id);
        self.maybe_start_completion_watcher(process_id, Some(notification_source));

        Ok(process_id)
    }

    /// Send rich user input items to an existing agent thread.
    pub(crate) async fn send_input(
        &self,
        agent_id: ProcessId,
        items: Vec<UserInput>,
    ) -> CodexResult<String> {
        let state = self.upgrade()?;
        let result = state
            .send_op(
                agent_id,
                Op::UserInput {
                    items,
                    final_output_json_schema: None,
                },
            )
            .await;
        if matches!(result, Err(ChaosErr::InternalAgentDied)) {
            let _ = state.remove_process(&agent_id).await;
            self.state.release_spawned_thread(agent_id);
        }
        result
    }

    /// Interrupt the current task for an existing agent thread.
    pub(crate) async fn interrupt_agent(&self, agent_id: ProcessId) -> CodexResult<String> {
        let state = self.upgrade()?;
        state.send_op(agent_id, Op::Interrupt).await
    }

    /// Submit a shutdown request to an existing agent thread.
    pub(crate) async fn shutdown_agent(&self, agent_id: ProcessId) -> CodexResult<String> {
        let state = self.upgrade()?;
        let result = state.send_op(agent_id, Op::Shutdown {}).await;
        let _ = state.remove_process(&agent_id).await;
        self.state.release_spawned_thread(agent_id);
        result
    }

    /// Fetch the last known status for `agent_id`, returning `NotFound` when unavailable.
    pub(crate) async fn get_status(&self, agent_id: ProcessId) -> AgentStatus {
        let Ok(state) = self.upgrade() else {
            // No agent available if upgrade fails.
            return AgentStatus::NotFound;
        };
        let Ok(thread) = state.get_process(agent_id).await else {
            return AgentStatus::NotFound;
        };
        thread.agent_status().await
    }

    pub(crate) async fn get_agent_nickname_and_role(
        &self,
        agent_id: ProcessId,
    ) -> Option<(Option<String>, Option<String>)> {
        let Ok(state) = self.upgrade() else {
            return None;
        };
        let Ok(thread) = state.get_process(agent_id).await else {
            return None;
        };
        let session_source = thread.config_snapshot().await.session_source;
        Some((
            session_source.get_nickname(),
            session_source.get_agent_role(),
        ))
    }

    /// Subscribe to status updates for `agent_id`, yielding the latest value and changes.
    pub(crate) async fn subscribe_status(
        &self,
        agent_id: ProcessId,
    ) -> CodexResult<watch::Receiver<AgentStatus>> {
        let state = self.upgrade()?;
        let thread = state.get_process(agent_id).await?;
        Ok(thread.subscribe_status())
    }

    pub(crate) async fn get_total_token_usage(&self, agent_id: ProcessId) -> Option<TokenUsage> {
        let Ok(state) = self.upgrade() else {
            return None;
        };
        let Ok(thread) = state.get_process(agent_id).await else {
            return None;
        };
        thread.total_token_usage().await
    }

    pub(crate) async fn format_environment_context_subagents(
        &self,
        parent_process_id: ProcessId,
    ) -> String {
        let Ok(state) = self.upgrade() else {
            return String::new();
        };

        let mut agents = Vec::new();
        for process_id in state.list_process_ids().await {
            let Ok(thread) = state.get_process(process_id).await else {
                continue;
            };
            let snapshot = thread.config_snapshot().await;
            let SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                parent_process_id: agent_parent_process_id,
                agent_nickname,
                ..
            }) = snapshot.session_source
            else {
                continue;
            };
            if agent_parent_process_id != parent_process_id {
                continue;
            }
            agents.push(format_subagent_context_line(
                &process_id.to_string(),
                agent_nickname.as_deref(),
            ));
        }
        agents.sort();
        agents.join("\n")
    }

    /// Starts a detached watcher for sub-agents spawned from another thread.
    ///
    /// This is only enabled for `SubAgentSource::ProcessSpawn`, where a parent thread exists and
    /// can receive completion notifications.
    fn maybe_start_completion_watcher(
        &self,
        child_process_id: ProcessId,
        session_source: Option<SessionSource>,
    ) {
        let Some(SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
            parent_process_id, ..
        })) = session_source
        else {
            return;
        };
        let control = self.clone();
        tokio::spawn(async move {
            let status = match control.subscribe_status(child_process_id).await {
                Ok(mut status_rx) => {
                    let mut status = status_rx.borrow().clone();
                    while !is_final(&status) {
                        if status_rx.changed().await.is_err() {
                            status = control.get_status(child_process_id).await;
                            break;
                        }
                        status = status_rx.borrow().clone();
                    }
                    status
                }
                Err(_) => control.get_status(child_process_id).await,
            };
            if !is_final(&status) {
                return;
            }

            let Ok(state) = control.upgrade() else {
                return;
            };
            let Ok(parent_thread) = state.get_process(parent_process_id).await else {
                return;
            };
            parent_thread
                .inject_user_message_without_turn(format_subagent_notification_message(
                    &child_process_id.to_string(),
                    &status,
                ))
                .await;
        });
    }

    fn upgrade(&self) -> CodexResult<Arc<ProcessTableState>> {
        self.manager
            .upgrade()
            .ok_or_else(|| ChaosErr::UnsupportedOperation("thread manager dropped".to_string()))
    }

    async fn inherited_shell_snapshot_for_source(
        &self,
        state: &Arc<ProcessTableState>,
        session_source: Option<&SessionSource>,
    ) -> Option<Arc<ShellSnapshot>> {
        let Some(SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
            parent_process_id, ..
        })) = session_source
        else {
            return None;
        };

        let parent_thread = state.get_process(*parent_process_id).await.ok()?;
        parent_thread.codex.session.user_shell().shell_snapshot()
    }
}
#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
