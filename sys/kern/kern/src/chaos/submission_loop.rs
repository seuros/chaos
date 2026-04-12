use std::sync::Arc;

use async_channel::Receiver;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ItemCompletedEvent;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::Submission;
use tracing::Instrument;
use tracing::debug;
use tracing::warn;

use crate::config::Config;
use crate::parse_turn_item;
use crate::protocol::Op;
use crate::tools::spec::ToolsConfig;
use crate::tools::spec::ToolsConfigParams;

use super::Session;
use super::SessionSettingsUpdate;
use super::TurnContext;
use super::TurnSkillsContext;

pub(crate) fn initial_replay_event_msgs(
    initial_history: &chaos_ipc::protocol::InitialHistory,
    process_id: ProcessId,
) -> Option<Vec<EventMsg>> {
    let mut events = Vec::new();
    for item in initial_history.get_rollout_items() {
        match item {
            RolloutItem::EventMsg(event) => events.push(event),
            RolloutItem::ResponseItem(response_item) => {
                if let Some(item) = parse_turn_item(&response_item) {
                    events.push(EventMsg::ItemCompleted(ItemCompletedEvent {
                        process_id,
                        turn_id: String::new(),
                        item,
                    }));
                }
            }
            RolloutItem::SessionMeta(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::Compacted(_) => {}
        }
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

pub(super) async fn submission_loop(
    sess: Arc<Session>,
    config: Arc<Config>,
    rx_sub: Receiver<Submission>,
) {
    // To break out of this loop, send Op::Shutdown.
    while let Ok(sub) = rx_sub.recv().await {
        debug!(?sub, "Submission");
        let dispatch_span = submission_dispatch_span(&sub);
        let should_exit = async {
            match sub.op.clone() {
                Op::Interrupt => {
                    handlers::interrupt(&sess).await;
                    false
                }
                Op::CleanBackgroundTerminals => {
                    handlers::clean_background_terminals(&sess).await;
                    false
                }
                Op::OverrideTurnContext {
                    cwd,
                    approval_policy,
                    approvals_reviewer,
                    sandbox_policy,
                    model,
                    effort,
                    summary,
                    service_tier,
                    collaboration_mode,
                    personality,
                } => {
                    let collaboration_mode = if let Some(collab_mode) = collaboration_mode {
                        collab_mode
                    } else {
                        let state = sess.state.lock().await;
                        state.session_configuration.collaboration_mode.with_updates(
                            model.clone(),
                            effort,
                            /*minion_instructions*/ None,
                        )
                    };
                    // If clamped and model changed, forward to claude subprocess.
                    if sess.services.model_client.is_clamped()
                        && let Some(ref model_slug) = model
                        && let Err(e) = sess.services.model_client.set_clamp_model(model_slug).await
                    {
                        warn!("failed to set clamped model: {e}");
                    }
                    handlers::override_turn_context(
                        &sess,
                        sub.id.clone(),
                        SessionSettingsUpdate {
                            cwd,
                            approval_policy,
                            approvals_reviewer,
                            sandbox_policy,
                            collaboration_mode: Some(collaboration_mode),
                            reasoning_summary: summary,
                            service_tier,
                            personality,
                            ..Default::default()
                        },
                    )
                    .await;
                    false
                }
                Op::SetClamped { enabled } => {
                    sess.services.model_client.set_clamped(enabled).await;
                    let mode = if enabled {
                        "clamped (Claude Code MAX)"
                    } else {
                        "direct API"
                    };
                    sess.send_event_raw(Event {
                        id: sub.id.clone(),
                        msg: EventMsg::AgentMessage(chaos_ipc::protocol::AgentMessageEvent {
                            message: format!("Transport switched to {mode}."),
                            phase: None,
                        }),
                    })
                    .await;
                    false
                }
                Op::UserInput { .. } | Op::UserTurn { .. } => {
                    handlers::user_input_or_turn(&sess, sub.id.clone(), sub.op).await;
                    false
                }
                Op::ExecApproval {
                    id: approval_id,
                    turn_id,
                    decision,
                } => {
                    handlers::exec_approval(&sess, approval_id, turn_id, decision).await;
                    false
                }
                Op::PatchApproval { id, decision } => {
                    handlers::patch_approval(&sess, id, decision).await;
                    false
                }
                Op::UserInputAnswer { id, response } => {
                    handlers::request_user_input_response(&sess, id, response).await;
                    false
                }
                Op::RequestPermissionsResponse { id, response } => {
                    handlers::request_permissions_response(&sess, id, response).await;
                    false
                }
                Op::DynamicToolResponse { id, response } => {
                    handlers::dynamic_tool_response(&sess, id, response).await;
                    false
                }
                Op::AddToHistory { text } => {
                    handlers::add_to_history(&sess, &config, text).await;
                    false
                }
                Op::GetHistoryEntryRequest { offset, log_id } => {
                    handlers::get_history_entry_request(
                        &sess,
                        &config,
                        sub.id.clone(),
                        offset,
                        log_id,
                    )
                    .await;
                    false
                }
                Op::ListMcpTools => {
                    handlers::list_mcp_tools(&sess, &config, sub.id.clone()).await;
                    false
                }
                Op::ListAllTools => {
                    handlers::list_all_tools(&sess, &config, sub.id.clone()).await;
                    false
                }
                Op::RefreshMcpServers { config } => {
                    handlers::refresh_mcp_servers(&sess, config).await;
                    false
                }
                Op::ReloadUserConfig => {
                    handlers::reload_user_config(&sess).await;
                    false
                }
                Op::ListCustomPrompts => {
                    handlers::list_custom_prompts(&sess, sub.id.clone()).await;
                    false
                }
                Op::ListSkills { cwds, force_reload } => {
                    handlers::list_skills(&sess, sub.id.clone(), cwds, force_reload).await;
                    false
                }
                Op::ListRemoteSkills {
                    hazelnut_scope,
                    product_surface,
                    enabled,
                } => {
                    handlers::list_remote_skills(
                        &sess,
                        &config,
                        sub.id.clone(),
                        hazelnut_scope,
                        product_surface,
                        enabled,
                    )
                    .await;
                    false
                }
                Op::DownloadRemoteSkill { hazelnut_id } => {
                    handlers::export_remote_skill(&sess, &config, sub.id.clone(), hazelnut_id)
                        .await;
                    false
                }
                Op::Undo => {
                    handlers::undo(&sess, sub.id.clone()).await;
                    false
                }
                Op::Compact => {
                    handlers::compact(&sess, sub.id.clone()).await;
                    false
                }
                Op::DropMemories | Op::UpdateMemories => {
                    // Memory subsystem evicted — these ops are now no-ops.
                    false
                }
                Op::ProcessRollback { num_turns } => {
                    handlers::process_rollback(&sess, sub.id.clone(), num_turns).await;
                    false
                }
                Op::SetProcessName { name } => {
                    handlers::set_process_name(&sess, sub.id.clone(), name).await;
                    false
                }
                Op::RunUserShellCommand { command } => {
                    handlers::run_user_shell_command(&sess, sub.id.clone(), command).await;
                    false
                }
                Op::ResolveElicitation {
                    server_name,
                    request_id,
                    decision,
                    content,
                    meta,
                } => {
                    handlers::resolve_elicitation(
                        &sess,
                        server_name,
                        request_id,
                        decision,
                        content,
                        meta,
                    )
                    .await;
                    false
                }
                Op::Shutdown => handlers::shutdown(&sess, sub.id.clone()).await,
                Op::Review { review_request } => {
                    handlers::review(&sess, &config, sub.id.clone(), review_request).await;
                    false
                }
                _ => false, // Ignore unknown ops; enum is non_exhaustive to allow extensions.
            }
        }
        .instrument(dispatch_span)
        .await;
        if should_exit {
            break;
        }
    }

    debug!("Agent loop exited");
}

pub(crate) fn submission_dispatch_span(sub: &Submission) -> tracing::Span {
    let op_name = sub.op.kind();
    let span_name = format!("op.dispatch.{op_name}");
    let dispatch_span = tracing::info_span!(
        "submission_dispatch",
        otel.name = span_name.as_str(),
        submission.id = sub.id.as_str(),
        chaos.op = op_name
    );
    if let Some(trace) = sub.trace.as_ref()
        && !chaos_syslog::set_parent_from_w3c_trace_context(&dispatch_span, trace)
    {
        warn!(
            submission.id = sub.id.as_str(),
            "ignoring invalid submission trace carrier"
        );
    }
    dispatch_span
}

/// Spawn a review thread using the given prompt.
pub(super) async fn spawn_review_thread(
    sess: Arc<Session>,
    config: Arc<Config>,
    parent_turn_context: Arc<TurnContext>,
    sub_id: String,
    resolved: crate::review_prompts::ResolvedReviewRequest,
) {
    use crate::models_manager::manager::RefreshStrategy;
    use crate::turn_metadata::TurnMetadataState;
    use crate::turn_timing::TurnTimingState;
    use chaos_ipc::config_types::WebSearchMode;
    use chaos_ipc::protocol::ReviewRequest;

    use chaos_syslog::current_span_trace_id;

    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| parent_turn_context.model_info.slug.clone());
    let review_model_info = sess
        .services
        .models_manager
        .get_model_info(&model, &config)
        .await;
    // For reviews, disable web_search and view_image regardless of global settings.
    let review_features = sess.features.clone();
    let review_web_search_mode = WebSearchMode::Disabled;
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &review_model_info,
        available_models: &sess
            .services
            .models_manager
            .list_models(RefreshStrategy::OnlineIfUncached)
            .await,
        features: &review_features,
        web_search_mode: Some(review_web_search_mode),
        session_source: parent_turn_context.session_source.clone(),
        sandbox_policy: parent_turn_context.sandbox_policy.get(),
        collab_enabled: config.collab_enabled,
    })
    .with_web_search_config(/*web_search_config*/ None)
    .with_allow_login_shell(config.permissions.allow_login_shell)
    .with_agent_roles(config.agent_roles.clone());

    let review_prompt = resolved.prompt.clone();
    let provider = parent_turn_context.provider.clone();
    let auth_manager = parent_turn_context.auth_manager.clone();
    let model_info = review_model_info.clone();

    // Build per-turn client with the requested model/family.
    let mut per_turn_config = (*config).clone();
    per_turn_config.model = Some(model.clone());
    per_turn_config.features = review_features.clone();
    if let Err(err) = per_turn_config.web_search_mode.set(review_web_search_mode) {
        let fallback_value = per_turn_config.web_search_mode.value();
        tracing::warn!(
            error = %err,
            ?review_web_search_mode,
            ?fallback_value,
            "review web_search_mode is disallowed by requirements; keeping constrained value"
        );
    }

    let session_telemetry = parent_turn_context
        .session_telemetry
        .clone()
        .with_model(model.as_str(), review_model_info.slug.as_str());
    let auth_manager_for_context = auth_manager.clone();
    let provider_for_context = provider.clone();
    let session_telemetry_for_context = session_telemetry.clone();
    let reasoning_effort = per_turn_config.model_reasoning_effort;
    let reasoning_summary = per_turn_config
        .model_reasoning_summary
        .unwrap_or(model_info.default_reasoning_summary);
    let session_source = parent_turn_context.session_source.clone();

    let per_turn_config = Arc::new(per_turn_config);
    let review_turn_id = sub_id.to_string();
    let turn_metadata_state = Arc::new(TurnMetadataState::new(
        review_turn_id.clone(),
        parent_turn_context.cwd.clone(),
        parent_turn_context.sandbox_policy.get(),
    ));

    let review_turn_context = TurnContext {
        sub_id: review_turn_id,
        trace_id: current_span_trace_id(),
        config: per_turn_config,
        auth_manager: auth_manager_for_context,
        model_info: model_info.clone(),
        session_telemetry: session_telemetry_for_context,
        provider: provider_for_context,
        reasoning_effort,
        reasoning_summary,
        session_source,
        tools_config,
        features: parent_turn_context.features.clone(),
        ghost_snapshot: parent_turn_context.ghost_snapshot.clone(),
        current_date: parent_turn_context.current_date.clone(),
        timezone: parent_turn_context.timezone.clone(),
        app_server_client_name: parent_turn_context.app_server_client_name.clone(),
        minion_instructions: None,
        user_instructions: None,
        compact_prompt: parent_turn_context.compact_prompt.clone(),
        collaboration_mode: parent_turn_context.collaboration_mode.clone(),
        personality: parent_turn_context.personality,
        approval_policy: parent_turn_context.approval_policy.clone(),
        sandbox_policy: parent_turn_context.sandbox_policy.clone(),
        file_system_sandbox_policy: parent_turn_context.file_system_sandbox_policy.clone(),
        network_sandbox_policy: parent_turn_context.network_sandbox_policy,
        network: parent_turn_context.network.clone(),
        shell_environment_policy: parent_turn_context.shell_environment_policy.clone(),
        cwd: parent_turn_context.cwd.clone(),
        final_output_json_schema: None,
        alcatraz_macos_exe: parent_turn_context.alcatraz_macos_exe.clone(),
        alcatraz_linux_exe: parent_turn_context.alcatraz_linux_exe.clone(),
        alcatraz_freebsd_exe: parent_turn_context.alcatraz_freebsd_exe.clone(),
        tool_call_gate: Arc::new(chaos_ready::ReadinessFlag::new()),
        dynamic_tools: parent_turn_context.dynamic_tools.clone(),
        truncation_policy: model_info.truncation_policy.into(),
        turn_metadata_state,
        turn_skills: TurnSkillsContext::new(parent_turn_context.turn_skills.outcome.clone()),
        turn_timing_state: Arc::new(TurnTimingState::default()),
    };

    // Seed the child task with the review prompt as the initial user message.
    let input: Vec<chaos_ipc::user_input::UserInput> =
        vec![chaos_ipc::user_input::UserInput::Text {
            text: review_prompt,
            // Review prompt is synthesized; no UI element ranges to preserve.
            text_elements: Vec::new(),
        }];
    let tc = Arc::new(review_turn_context);
    tc.turn_metadata_state.spawn_git_enrichment_task();
    // TODO(ccunningham): Review turns currently rely on `spawn_task` for TurnComplete but do not
    // emit a parent TurnStarted. Consider giving review a full parent turn lifecycle
    // (TurnStarted + TurnComplete) for consistency with other standalone tasks.
    sess.spawn_task(tc.clone(), input, crate::tasks::ReviewTask::new())
        .await;

    // Announce entering review mode so UIs can switch modes.
    let review_request = ReviewRequest {
        target: resolved.target,
        user_facing_hint: Some(resolved.user_facing_hint),
    };
    sess.send_event(&tc, EventMsg::EnteredReviewMode(review_request))
        .await;
}

/// Operation handlers
pub(crate) mod handlers {
    use crate::chaos::Session;
    use crate::chaos::SessionSettingsUpdate;
    use crate::chaos::SteerInputError;

    use crate::chaos::submission_loop::spawn_review_thread;
    use crate::config::Config;

    use crate::mcp::auth::compute_auth_statuses;
    use crate::mcp::collect_mcp_snapshot_from_manager;
    use crate::review_prompts::resolve_review_request;
    use crate::rollout::RolloutRecorder;
    use crate::rollout::process_names;
    use crate::tasks::CompactTask;
    use crate::tasks::UndoTask;
    use crate::tasks::UserShellCommandMode;
    use crate::tasks::UserShellCommandTask;
    use crate::tasks::execute_user_shell_command;
    use chaos_ipc::custom_prompts::CustomPrompt;
    use chaos_ipc::protocol::ChaosErrorInfo;
    use chaos_ipc::protocol::ErrorEvent;
    use chaos_ipc::protocol::Event;
    use chaos_ipc::protocol::EventMsg;
    use chaos_ipc::protocol::InitialHistory;
    use chaos_ipc::protocol::ListCustomPromptsResponseEvent;
    use chaos_ipc::protocol::ListRemoteSkillsResponseEvent;
    use chaos_ipc::protocol::ListSkillsResponseEvent;
    use chaos_ipc::protocol::McpServerRefreshConfig;
    use chaos_ipc::protocol::Op;
    use chaos_ipc::protocol::ProcessNameUpdatedEvent;
    use chaos_ipc::protocol::ProcessRolledBackEvent;
    use chaos_ipc::protocol::RemoteSkillDownloadedEvent;
    use chaos_ipc::protocol::RemoteSkillHazelnutScope;
    use chaos_ipc::protocol::RemoteSkillProductSurface;
    use chaos_ipc::protocol::RemoteSkillSummary;
    use chaos_ipc::protocol::ResumedHistory;
    use chaos_ipc::protocol::ReviewDecision;
    use chaos_ipc::protocol::ReviewRequest;
    use chaos_ipc::protocol::RolloutItem;
    use chaos_ipc::protocol::SkillsListEntry;
    use chaos_ipc::protocol::TurnAbortReason;
    use chaos_ipc::protocol::WarningEvent;
    use chaos_ipc::request_permissions::RequestPermissionsResponse;
    use chaos_ipc::request_user_input::RequestUserInputResponse;

    use crate::context_manager::is_user_turn_boundary;
    use chaos_ipc::config_types::CollaborationMode;
    use chaos_ipc::config_types::ModeKind;
    use chaos_ipc::config_types::Settings;
    use chaos_ipc::dynamic_tools::DynamicToolResponse;
    use chaos_ipc::mcp::RequestId as ProtocolRequestId;
    use chaos_ipc::user_input::UserInput;
    use chaos_mcp_runtime::ElicitationAction;
    use chaos_mcp_runtime::ElicitationResponse;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tracing::info;
    use tracing::warn;

    use super::super::skills_info::errors_to_info;
    use super::super::skills_info::skills_to_info;

    pub async fn interrupt(sess: &Arc<Session>) {
        sess.interrupt_task().await;
    }

    pub async fn clean_background_terminals(sess: &Arc<Session>) {
        sess.close_unified_exec_processes().await;
    }

    pub async fn override_turn_context(
        sess: &Session,
        sub_id: String,
        updates: SessionSettingsUpdate,
    ) {
        if let Err(err) = sess.update_settings(updates).await {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: err.to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::BadRequest),
                }),
            })
            .await;
        }
    }

    pub async fn user_input_or_turn(sess: &Arc<Session>, sub_id: String, op: Op) {
        let (items, updates) = match op {
            Op::UserTurn {
                cwd,
                approval_policy,
                sandbox_policy,
                model,
                effort,
                summary,
                service_tier,
                final_output_json_schema,
                items,
                collaboration_mode,
                personality,
            } => {
                let collaboration_mode = collaboration_mode.or_else(|| {
                    Some(CollaborationMode {
                        mode: ModeKind::Default,
                        settings: Settings {
                            model: model.clone(),
                            reasoning_effort: effort,
                            minion_instructions: None,
                        },
                    })
                });
                (
                    items,
                    SessionSettingsUpdate {
                        cwd: Some(cwd),
                        approval_policy: Some(approval_policy),
                        approvals_reviewer: None,
                        sandbox_policy: Some(sandbox_policy),
                        collaboration_mode,
                        reasoning_summary: summary,
                        service_tier,
                        final_output_json_schema: Some(final_output_json_schema),
                        personality,
                        app_server_client_name: None,
                    },
                )
            }
            Op::UserInput {
                items,
                final_output_json_schema,
            } => (
                items,
                SessionSettingsUpdate {
                    final_output_json_schema: Some(final_output_json_schema),
                    ..Default::default()
                },
            ),
            _ => unreachable!(),
        };

        let Ok(current_context) = sess.new_turn_with_sub_id(sub_id, updates).await else {
            // new_turn_with_sub_id already emits the error event.
            return;
        };
        current_context.session_telemetry.user_prompt(&items);

        // Attempt to inject input into current task.
        if let Err(SteerInputError::NoActiveTurn(items)) =
            sess.steer_input(items, /*expected_turn_id*/ None).await
        {
            sess.refresh_mcp_servers_if_requested(&current_context)
                .await;
            sess.spawn_task(
                Arc::clone(&current_context),
                items,
                crate::tasks::RegularTask,
            )
            .await;
        }
    }

    pub async fn run_user_shell_command(sess: &Arc<Session>, sub_id: String, command: String) {
        if let Some((turn_context, cancellation_token)) =
            sess.active_turn_context_and_cancellation_token().await
        {
            let session = Arc::clone(sess);
            tokio::spawn(async move {
                execute_user_shell_command(
                    session,
                    turn_context,
                    command,
                    cancellation_token,
                    UserShellCommandMode::ActiveTurnAuxiliary,
                )
                .await;
            });
            return;
        }

        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
        sess.spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            UserShellCommandTask::new(command),
        )
        .await;
    }

    pub async fn resolve_elicitation(
        sess: &Arc<Session>,
        server_name: String,
        request_id: ProtocolRequestId,
        decision: chaos_ipc::approvals::ElicitationAction,
        content: Option<Value>,
        meta: Option<Value>,
    ) {
        let action = match decision {
            chaos_ipc::approvals::ElicitationAction::Accept => ElicitationAction::Accept,
            chaos_ipc::approvals::ElicitationAction::Decline => ElicitationAction::Decline,
            chaos_ipc::approvals::ElicitationAction::Cancel => ElicitationAction::Cancel,
        };
        let content = match action {
            // Preserve the legacy fallback for clients that only send an action.
            ElicitationAction::Accept => Some(content.unwrap_or_else(|| serde_json::json!({}))),
            ElicitationAction::Decline | ElicitationAction::Cancel => None,
        };
        let response = ElicitationResponse {
            action,
            content,
            meta,
        };
        let request_id = chaos_mcp_runtime::manager::protocol_request_id_to_guest(&request_id);
        if let Err(err) = sess
            .resolve_elicitation(server_name, request_id, response)
            .await
        {
            warn!(
                error = %err,
                "failed to resolve elicitation request in session"
            );
        }
    }

    /// Propagate a user's exec approval decision to the session.
    /// Also optionally applies an execpolicy amendment.
    pub async fn exec_approval(
        sess: &Arc<Session>,
        approval_id: String,
        turn_id: Option<String>,
        decision: ReviewDecision,
    ) {
        let event_turn_id = turn_id.unwrap_or_else(|| approval_id.clone());
        if let ReviewDecision::ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment,
        } = &decision
        {
            match sess
                .persist_execpolicy_amendment(proposed_execpolicy_amendment)
                .await
            {
                Ok(()) => {
                    sess.record_execpolicy_amendment_message(
                        &event_turn_id,
                        proposed_execpolicy_amendment,
                    )
                    .await;
                }
                Err(err) => {
                    let message = format!("Failed to apply execpolicy amendment: {err}");
                    tracing::warn!("{message}");
                    let warning = EventMsg::Warning(WarningEvent { message });
                    sess.send_event_raw(Event {
                        id: event_turn_id.clone(),
                        msg: warning,
                    })
                    .await;
                }
            }
        }
        match decision {
            ReviewDecision::Abort => {
                sess.interrupt_task().await;
            }
            other => sess.notify_approval(&approval_id, other).await,
        }
    }

    pub async fn patch_approval(sess: &Arc<Session>, id: String, decision: ReviewDecision) {
        match decision {
            ReviewDecision::Abort => {
                sess.interrupt_task().await;
            }
            other => sess.notify_approval(&id, other).await,
        }
    }

    pub async fn request_user_input_response(
        sess: &Arc<Session>,
        id: String,
        response: RequestUserInputResponse,
    ) {
        sess.notify_user_input_response(&id, response).await;
    }

    pub async fn request_permissions_response(
        sess: &Arc<Session>,
        id: String,
        response: RequestPermissionsResponse,
    ) {
        sess.notify_request_permissions_response(&id, response)
            .await;
    }

    pub async fn dynamic_tool_response(
        sess: &Arc<Session>,
        id: String,
        response: DynamicToolResponse,
    ) {
        sess.notify_dynamic_tool_response(&id, response).await;
    }

    pub async fn add_to_history(sess: &Arc<Session>, config: &Arc<Config>, text: String) {
        let id = sess.conversation_id;
        let config = Arc::clone(config);
        let runtime_db = sess.services.runtime_db.clone();
        tokio::spawn(async move {
            if let Err(e) =
                crate::message_history::append_entry(&text, &id, runtime_db.as_deref(), &config)
                    .await
            {
                warn!("failed to append to message history: {e}");
            }
        });
    }

    pub async fn get_history_entry_request(
        sess: &Arc<Session>,
        config: &Arc<Config>,
        sub_id: String,
        offset: usize,
        log_id: u64,
    ) {
        let sess_clone = Arc::clone(sess);
        let runtime_db = sess.services.runtime_db.clone();
        let _config = Arc::clone(config);

        tokio::spawn(async move {
            let entry_opt =
                crate::message_history::lookup(log_id, offset, runtime_db.as_deref()).await;

            let event = Event {
                id: sub_id,
                msg: EventMsg::GetHistoryEntryResponse(
                    crate::protocol::GetHistoryEntryResponseEvent {
                        offset,
                        log_id,
                        entry: entry_opt,
                    },
                ),
            };

            sess_clone.send_event_raw(event).await;
        });
    }

    pub async fn refresh_mcp_servers(sess: &Arc<Session>, refresh_config: McpServerRefreshConfig) {
        let mut guard = sess.pending_mcp_server_refresh_config.lock().await;
        *guard = Some(refresh_config);
    }

    pub async fn reload_user_config(sess: &Arc<Session>) {
        sess.reload_user_config_layer().await;
    }

    pub async fn list_all_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
        use chaos_ipc::protocol::AllToolsResponseEvent;
        use chaos_ipc::protocol::ToolSummary;

        let mut tools: Vec<ToolSummary> = {
            let catalog = sess
                .services
                .catalog
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            catalog
                .tools()
                .iter()
                .map(|(source, tool)| {
                    let source_str = match source {
                        crate::catalog::CatalogSource::Module(name) => name.clone(),
                        crate::catalog::CatalogSource::Mcp(name) => format!("mcp:{name}"),
                    };
                    let annotation_labels = tool
                        .annotations
                        .as_ref()
                        .and_then(|v| {
                            serde_json::from_value::<chaos_mcp_runtime::ToolAnnotations>(v.clone())
                                .ok()
                        })
                        .map(|ann| {
                            let mut labels = crate::tools::spec::annotation_labels(&ann);
                            let has_read_semantics = labels
                                .iter()
                                .any(|label| label == "read-only" || label == "writes");
                            if !has_read_semantics && let Some(read_only) = tool.read_only_hint {
                                labels.push(
                                    if read_only { "read-only" } else { "writes" }.to_string(),
                                );
                            }
                            labels
                        })
                        .or_else(|| {
                            tool.read_only_hint.map(|read_only| {
                                vec![if read_only { "read-only" } else { "writes" }.to_string()]
                            })
                        })
                        .unwrap_or_default();
                    ToolSummary {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        annotation_labels,
                        annotations: tool.annotations.clone(),
                        source: source_str,
                    }
                })
                .collect()
        };

        // Include script-defined tools from the hallucinate engine.
        if let Some(ref handle) = sess.services.hallucinate {
            for tool in handle.list_tools().await {
                tools.push(ToolSummary {
                    name: tool.name,
                    description: tool.description,
                    annotation_labels: Vec::new(),
                    annotations: None,
                    source: "hallucinate".to_string(),
                });
            }
        }

        let event = Event {
            id: sub_id,
            msg: EventMsg::AllToolsResponse(AllToolsResponseEvent { tools }),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_mcp_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
        let mcp_connection_manager = sess.services.mcp_connection_manager.read().await;
        let _auth = sess.services.auth_manager.auth().await;
        let config = sess.get_config().await;
        let mcp_servers = sess.services.mcp_manager.effective_servers(&config);
        let snapshot = collect_mcp_snapshot_from_manager(
            &mcp_connection_manager,
            compute_auth_statuses(mcp_servers.iter(), config.mcp_oauth_credentials_store_mode)
                .await,
        )
        .await;
        let event = Event {
            id: sub_id,
            msg: EventMsg::McpListToolsResponse(snapshot),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_custom_prompts(sess: &Session, sub_id: String) {
        let custom_prompts: Vec<CustomPrompt> =
            if let Some(dir) = crate::custom_prompts::default_prompts_dir() {
                crate::custom_prompts::discover_prompts_in(&dir).await
            } else {
                Vec::new()
            };

        let event = Event {
            id: sub_id,
            msg: EventMsg::ListCustomPromptsResponse(ListCustomPromptsResponseEvent {
                custom_prompts,
            }),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_skills(
        sess: &Session,
        sub_id: String,
        cwds: Vec<PathBuf>,
        force_reload: bool,
    ) {
        let cwds = if cwds.is_empty() {
            let state = sess.state.lock().await;
            vec![state.session_configuration.cwd.clone()]
        } else {
            cwds
        };

        let skills_manager = &sess.services.skills_manager;
        let mut skills = Vec::new();
        for cwd in cwds {
            let outcome = skills_manager.skills_for_cwd(&cwd, force_reload).await;
            let errors = errors_to_info(&outcome.errors);
            let skills_metadata = skills_to_info(&outcome.skills, &outcome.disabled_paths);
            skills.push(SkillsListEntry {
                cwd,
                skills: skills_metadata,
                errors,
            });
        }

        let event = Event {
            id: sub_id,
            msg: EventMsg::ListSkillsResponse(ListSkillsResponseEvent { skills }),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_remote_skills(
        sess: &Session,
        config: &Arc<Config>,
        sub_id: String,
        hazelnut_scope: RemoteSkillHazelnutScope,
        product_surface: RemoteSkillProductSurface,
        enabled: Option<bool>,
    ) {
        let auth = sess.services.auth_manager.auth().await;
        let response = crate::skills::remote::list_remote_skills(
            config,
            auth.as_ref(),
            hazelnut_scope,
            product_surface,
            enabled,
        )
        .await
        .map(|skills| {
            skills
                .into_iter()
                .map(|skill| RemoteSkillSummary {
                    id: skill.id,
                    name: skill.name,
                    description: skill.description,
                })
                .collect::<Vec<_>>()
        });

        match response {
            Ok(skills) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::ListRemoteSkillsResponse(ListRemoteSkillsResponseEvent {
                        skills,
                    }),
                };
                sess.send_event_raw(event).await;
            }
            Err(err) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("failed to list remote skills: {err}"),
                        chaos_error_info: Some(ChaosErrorInfo::Other),
                    }),
                };
                sess.send_event_raw(event).await;
            }
        }
    }

    pub async fn export_remote_skill(
        sess: &Session,
        config: &Arc<Config>,
        sub_id: String,
        hazelnut_id: String,
    ) {
        let auth = sess.services.auth_manager.auth().await;
        match crate::skills::remote::export_remote_skill(
            config,
            auth.as_ref(),
            hazelnut_id.as_str(),
        )
        .await
        {
            Ok(result) => {
                let id = result.id;
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::RemoteSkillDownloaded(RemoteSkillDownloadedEvent {
                        id: id.clone(),
                        name: id,
                        path: result.path,
                    }),
                };
                sess.send_event_raw(event).await;
            }
            Err(err) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("failed to export remote skill {hazelnut_id}: {err}"),
                        chaos_error_info: Some(ChaosErrorInfo::Other),
                    }),
                };
                sess.send_event_raw(event).await;
            }
        }
    }

    pub async fn undo(sess: &Arc<Session>, sub_id: String) {
        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
        sess.spawn_task(turn_context, Vec::new(), UndoTask::new())
            .await;
    }

    pub async fn compact(sess: &Arc<Session>, sub_id: String) {
        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;

        sess.spawn_task(
            Arc::clone(&turn_context),
            vec![UserInput::Text {
                text: turn_context.compact_prompt().to_string(),
                // Compaction prompt is synthesized; no UI element ranges to preserve.
                text_elements: Vec::new(),
            }],
            CompactTask,
        )
        .await;
    }

    pub async fn process_rollback(sess: &Arc<Session>, sub_id: String, num_turns: u32) {
        if num_turns == 0 {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "num_turns must be >= 1".to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        }

        let has_active_turn = { sess.active_turn.lock().await.is_some() };
        if has_active_turn {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "Cannot rollback while a turn is in progress.".to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        }

        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
        let recorder = {
            let guard = sess.services.rollout.lock().await;
            guard.clone()
        };
        let Some(recorder) = recorder else {
            sess.send_event_raw(Event {
                id: turn_context.sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: "thread rollback requires persisted session history".to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        };
        if let Err(err) = recorder.flush().await {
            sess.send_event_raw(Event {
                id: turn_context.sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: format!(
                        "failed to flush persisted session history for rollback replay: {err}"
                    ),
                    chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        }

        let initial_history = match RolloutRecorder::get_rollout_history_for_process(
            sess.conversation_id,
        )
        .await
        {
            Ok(history) => history,
            Err(err) => {
                let live_rollout_items = recorder.snapshot_rollout_items();
                if live_rollout_items.is_empty() {
                    sess.send_event_raw(Event {
                                id: turn_context.sub_id.clone(),
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!(
                                        "failed to load persisted session history for rollback replay: {err}"
                                    ),
                                    chaos_error_info: Some(ChaosErrorInfo::ProcessRollbackFailed),
                                }),
                            })
                            .await;
                    return;
                }
                InitialHistory::Resumed(ResumedHistory {
                    conversation_id: sess.conversation_id,
                    history: live_rollout_items,
                })
            }
        };

        let rollback_event = ProcessRolledBackEvent { num_turns };
        let rollback_msg = EventMsg::ProcessRolledBack(rollback_event.clone());
        let replay_items = initial_history
            .get_rollout_items()
            .into_iter()
            .chain(std::iter::once(RolloutItem::EventMsg(rollback_msg.clone())))
            .collect::<Vec<_>>();
        sess.persist_rollout_items(&[RolloutItem::EventMsg(rollback_msg.clone())])
            .await;
        sess.flush_rollout().await;
        sess.apply_rollout_reconstruction(turn_context.as_ref(), replay_items.as_slice())
            .await;
        sess.recompute_token_usage(turn_context.as_ref()).await;

        sess.deliver_event_raw(Event {
            id: turn_context.sub_id.clone(),
            msg: rollback_msg,
        })
        .await;
    }

    /// Persists the explicit process name in SQLite, updates in-memory state, and emits
    /// a `ProcessNameUpdated` event on success.
    /// It then updates `SessionConfiguration::process_name`.
    /// Returns an error event if the name is empty or session persistence is disabled.
    pub async fn set_process_name(sess: &Arc<Session>, sub_id: String, name: String) {
        let Some(name) = crate::util::normalize_process_name(&name) else {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "Process name cannot be empty.".to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::BadRequest),
                }),
            };
            sess.send_event_raw(event).await;
            return;
        };

        let persistence_enabled = {
            let rollout = sess.services.rollout.lock().await;
            rollout.is_some()
        };
        if !persistence_enabled {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "Session persistence is disabled; cannot rename process.".to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
            return;
        };

        let chaos_home = sess.chaos_home().await;
        if let Err(e) =
            process_names::append_process_name(&chaos_home, sess.conversation_id, &name).await
        {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: format!("Failed to set process name: {e}"),
                    chaos_error_info: Some(ChaosErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
            return;
        }

        {
            let mut state = sess.state.lock().await;
            state.session_configuration.process_name = Some(name.clone());
        }

        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::ProcessNameUpdated(ProcessNameUpdatedEvent {
                process_id: sess.conversation_id,
                process_name: Some(name),
            }),
        })
        .await;
    }

    pub async fn shutdown(sess: &Arc<Session>, sub_id: String) -> bool {
        sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
        sess.services
            .unified_exec_manager
            .terminate_all_processes()
            .await;
        info!("Shutting down Chaos instance");
        let history = sess.clone_history().await;
        let turn_count = history
            .raw_items()
            .iter()
            .filter(|item| is_user_turn_boundary(item))
            .count();
        sess.services.session_telemetry.counter(
            "chaos.conversation.turn.count",
            i64::try_from(turn_count).unwrap_or(0),
            &[],
        );

        // Gracefully flush and shutdown the session history recorder on session end.
        let recorder_opt = {
            let mut guard = sess.services.rollout.lock().await;
            guard.take()
        };
        if let Some(rec) = recorder_opt
            && let Err(e) = rec.shutdown().await
        {
            warn!("failed to shutdown rollout recorder: {e}");
            let event = Event {
                id: sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: "Failed to shutdown rollout recorder".to_string(),
                    chaos_error_info: Some(ChaosErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
        }

        let event = Event {
            id: sub_id,
            msg: EventMsg::ShutdownComplete,
        };
        sess.send_event_raw(event).await;
        true
    }

    pub async fn review(
        sess: &Arc<Session>,
        config: &Arc<Config>,
        sub_id: String,
        review_request: ReviewRequest,
    ) {
        let turn_context = sess.new_default_turn_with_sub_id(sub_id.clone()).await;
        sess.refresh_mcp_servers_if_requested(&turn_context).await;
        match resolve_review_request(review_request, turn_context.cwd.as_path()) {
            Ok(resolved) => {
                spawn_review_thread(
                    Arc::clone(sess),
                    Arc::clone(config),
                    turn_context.clone(),
                    sub_id,
                    resolved,
                )
                .await;
            }
            Err(err) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: err.to_string(),
                        chaos_error_info: Some(ChaosErrorInfo::Other),
                    }),
                };
                sess.send_event(&turn_context, event.msg).await;
            }
        }
    }
}
