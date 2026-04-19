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

pub(crate) mod handlers;

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
        file_system_sandbox_policy: &parent_turn_context.file_system_sandbox_policy,
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
        &parent_turn_context.file_system_sandbox_policy,
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
