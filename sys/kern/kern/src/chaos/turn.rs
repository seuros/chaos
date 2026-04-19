use std::collections::HashMap;
use std::sync::Arc;

use chaos_dtrace::HookEvent;
use chaos_dtrace::HookEventAfterAgent;
use chaos_dtrace::HookPayload;
use chaos_dtrace::HookResult;
use chaos_epoll::OrCancelExt;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::TurnStartedEvent;
use chaos_ipc::protocol::WarningEvent;
use chaos_ipc::user_input::UserInput;
use jiff::Timestamp;
use tokio_util::sync::CancellationToken;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::client::ModelClientSession;
use crate::compact::InitialContextInjection;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use crate::parse_turn_item;
use crate::prompt_images::response_input_item_from_user_input;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::tools::ToolRouter;
use crate::tools::router::ToolRouterParams;
use crate::turn_diff_tracker::TurnDiffTracker;

use super::PreviousTurnSettings;
use super::Session;
use super::TurnContext;

mod execution;
mod preparation;
mod sampling;

use preparation::run_auto_compact;
use preparation::run_pre_sampling_compact;
use sampling::run_sampling_request;

#[derive(Debug)]
pub(super) struct SamplingRequestResult {
    pub(super) needs_follow_up: bool,
    pub(super) last_agent_message: Option<String>,
}

pub(crate) fn get_last_assistant_message_from_turn(responses: &[ResponseItem]) -> Option<String> {
    for item in responses.iter().rev() {
        if let Some(message) = last_assistant_message_from_item(item, /*plan_mode*/ false) {
            return Some(message);
        }
    }
    None
}

/// Takes a user message as input and runs a loop where, at each sampling
/// request, the model replies with either:
///
/// - requested function calls
/// - an assistant message
///
/// While it is possible for the model to return multiple of these items in a
/// single sampling request, in practice, we generally one item per sampling
/// request:
///
/// - If the model requests a function call, we execute it and send the output
///   back to the model in the next sampling request.
/// - If the model sends only an assistant message, we record it in the
///   conversation history and consider the turn complete.
///
pub(crate) async fn run_turn(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
    prewarmed_client_session: Option<ModelClientSession>,
    cancellation_token: CancellationToken,
) -> Option<String> {
    if input.is_empty() {
        return None;
    }

    let model_info = turn_context.model_info.clone();
    let auto_compact_limit = model_info.auto_compact_token_limit().unwrap_or(i64::MAX);

    let event = EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_context.sub_id.clone(),
        model_context_window: turn_context.model_context_window(),
        collaboration_mode_kind: turn_context.collaboration_mode.mode,
    });
    sess.send_event(&turn_context, event).await;
    // TODO(ccunningham): Pre-turn compaction runs before context updates and the
    // new user message are recorded. Estimate pending incoming items (context
    // diffs/full reinjection + user input) and trigger compaction preemptively
    // when they would push the thread over the compaction threshold.
    if run_pre_sampling_compact(&sess, &turn_context)
        .await
        .is_err()
    {
        error!("Failed to run pre-sampling compact");
        return None;
    }

    sess.record_context_updates_and_set_reference_context_item(turn_context.as_ref())
        .await;

    let _mcp_tools = if turn_context.apps_enabled() {
        match sess
            .services
            .mcp_connection_manager
            .read()
            .await
            .list_all_tools()
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(mcp_tools) => mcp_tools,
            Err(_) => return None,
        }
    } else {
        HashMap::new()
    };
    let initial_input_for_turn: ResponseInputItem =
        response_input_item_from_user_input(input.clone());
    let response_item: ResponseItem = initial_input_for_turn.clone().into();
    sess.record_user_prompt_and_emit_turn_item(turn_context.as_ref(), &input, response_item)
        .await;
    // Track the previous-turn baseline from the regular user-turn path only so
    // standalone tasks (compact/shell/review/undo) cannot suppress future
    // model injections.
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
    }))
    .await;

    sess.maybe_start_ghost_snapshot(Arc::clone(&turn_context), cancellation_token.child_token())
        .await;
    let mut last_agent_message: Option<String> = None;
    let mut stop_hook_active = false;
    // Although from the perspective of chaos.rs, TurnDiffTracker has the lifecycle of a Task
    // which contains many turns, from the perspective of the user, it is a single turn.
    let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let mut server_model_warning_emitted_for_turn = false;

    // `ModelClientSession` is turn-scoped and caches WebSocket + sticky routing state, so we
    // reuse one instance across retries within this turn.
    let mut client_session =
        prewarmed_client_session.unwrap_or_else(|| sess.services.model_client.new_session());

    loop {
        if let Some(session_start_source) = sess.take_pending_session_start_source().await {
            let session_start_permission_mode = match turn_context.approval_policy.value() {
                ApprovalPolicy::Headless => "bypassPermissions",
                ApprovalPolicy::Supervised
                | ApprovalPolicy::Interactive
                | ApprovalPolicy::Granular(_) => "default",
            }
            .to_string();
            let session_start_request = chaos_dtrace::SessionStartRequest {
                session_id: sess.conversation_id,
                cwd: turn_context.cwd.clone(),
                transcript_path: None,
                model: turn_context.model_info.slug.clone(),
                permission_mode: session_start_permission_mode,
                source: session_start_source,
            };
            for run in sess.hooks().preview_session_start(&session_start_request) {
                sess.send_event(
                    &turn_context,
                    EventMsg::HookStarted(crate::protocol::HookStartedEvent {
                        turn_id: Some(turn_context.sub_id.clone()),
                        run,
                    }),
                )
                .await;
            }
            let session_start_outcome = sess
                .hooks()
                .run_session_start(session_start_request, Some(turn_context.sub_id.clone()))
                .await;
            for completed in session_start_outcome.hook_events {
                sess.send_event(&turn_context, EventMsg::HookCompleted(completed))
                    .await;
            }
            if session_start_outcome.should_stop {
                break;
            }
            if let Some(additional_context) = session_start_outcome.additional_context {
                let developer_message: ResponseItem =
                    DeveloperInstructions::new(additional_context).into();
                sess.record_conversation_items(
                    &turn_context,
                    std::slice::from_ref(&developer_message),
                )
                .await;
            }
        }

        // Note that pending_input would be something like a message the user
        // submitted through the UI while the model was running. Though the UI
        // may support this, the model might not.
        let pending_response_items = sess
            .get_pending_input()
            .await
            .into_iter()
            .map(ResponseItem::from)
            .collect::<Vec<ResponseItem>>();

        if !pending_response_items.is_empty() {
            for response_item in pending_response_items {
                if let Some(TurnItem::UserMessage(user_message)) = parse_turn_item(&response_item) {
                    // TODO: move pending input to be UserInput only to keep TextElements.
                    sess.record_user_prompt_and_emit_turn_item(
                        turn_context.as_ref(),
                        &user_message.content,
                        response_item,
                    )
                    .await;
                } else {
                    sess.record_conversation_items(
                        &turn_context,
                        std::slice::from_ref(&response_item),
                    )
                    .await;
                }
            }
        }

        // Construct the input that we will send to the model.
        let sampling_request_input: Vec<ResponseItem> = {
            sess.clone_history()
                .await
                .for_prompt(&turn_context.model_info.input_modalities)
        };

        let sampling_request_input_messages = sampling_request_input
            .iter()
            .filter_map(|item| match parse_turn_item(item) {
                Some(TurnItem::UserMessage(user_message)) => Some(user_message),
                _ => None,
            })
            .map(|user_message| user_message.message())
            .collect::<Vec<String>>();
        let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
        match run_sampling_request(
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            Arc::clone(&turn_diff_tracker),
            &mut client_session,
            turn_metadata_header.as_deref(),
            sampling_request_input,
            &mut server_model_warning_emitted_for_turn,
            cancellation_token.child_token(),
        )
        .await
        {
            Ok(sampling_request_output) => {
                let SamplingRequestResult {
                    needs_follow_up,
                    last_agent_message: sampling_request_last_agent_message,
                } = sampling_request_output;
                let total_usage_tokens = sess.get_total_token_usage().await;
                let token_limit_reached = total_usage_tokens >= auto_compact_limit;

                let estimated_token_count =
                    sess.get_estimated_token_count(turn_context.as_ref()).await;

                tracing::trace!(
                    turn_id = %turn_context.sub_id,
                    total_usage_tokens,
                    estimated_token_count = ?estimated_token_count,
                    auto_compact_limit,
                    token_limit_reached,
                    needs_follow_up,
                    "post sampling token usage"
                );

                // as long as compaction works well in getting us way below the token limit, we
                // shouldn't worry about being in an infinite loop.
                if token_limit_reached && needs_follow_up {
                    if run_auto_compact(
                        &sess,
                        &turn_context,
                        InitialContextInjection::BeforeLastUserMessage,
                    )
                    .await
                    .is_err()
                    {
                        return None;
                    }
                    continue;
                }

                if !needs_follow_up {
                    last_agent_message = sampling_request_last_agent_message;
                    let stop_hook_permission_mode = match turn_context.approval_policy.value() {
                        ApprovalPolicy::Headless => "bypassPermissions",
                        ApprovalPolicy::Supervised
                        | ApprovalPolicy::Interactive
                        | ApprovalPolicy::Granular(_) => "default",
                    }
                    .to_string();
                    let stop_request = chaos_dtrace::StopRequest {
                        session_id: sess.conversation_id,
                        turn_id: turn_context.sub_id.clone(),
                        cwd: turn_context.cwd.clone(),
                        transcript_path: None,
                        model: turn_context.model_info.slug.clone(),
                        permission_mode: stop_hook_permission_mode,
                        stop_hook_active,
                        last_assistant_message: last_agent_message.clone(),
                    };
                    for run in sess.hooks().preview_stop(&stop_request) {
                        sess.send_event(
                            &turn_context,
                            EventMsg::HookStarted(crate::protocol::HookStartedEvent {
                                turn_id: Some(turn_context.sub_id.clone()),
                                run,
                            }),
                        )
                        .await;
                    }
                    let stop_outcome = sess.hooks().run_stop(stop_request).await;
                    for completed in stop_outcome.hook_events {
                        sess.send_event(&turn_context, EventMsg::HookCompleted(completed))
                            .await;
                    }
                    if stop_outcome.should_block {
                        if let Some(continuation_prompt) = stop_outcome.continuation_prompt.clone()
                        {
                            let developer_message: ResponseItem =
                                DeveloperInstructions::new(continuation_prompt).into();
                            sess.record_conversation_items(
                                &turn_context,
                                std::slice::from_ref(&developer_message),
                            )
                            .await;
                            stop_hook_active = true;
                            continue;
                        } else {
                            sess.send_event(
                                &turn_context,
                                EventMsg::Warning(WarningEvent {
                                    message: "Stop hook requested continuation without a prompt; ignoring the block.".to_string(),
                                }),
                            )
                            .await;
                        }
                    }
                    if stop_outcome.should_stop {
                        break;
                    }
                    let hook_outcomes = sess
                        .hooks()
                        .dispatch(HookPayload {
                            session_id: sess.conversation_id,
                            cwd: turn_context.cwd.clone(),
                            client: turn_context.app_server_client_name.clone(),
                            triggered_at: Timestamp::now(),
                            hook_event: HookEvent::AfterAgent {
                                event: HookEventAfterAgent {
                                    process_id: sess.conversation_id,
                                    turn_id: turn_context.sub_id.clone(),
                                    input_messages: sampling_request_input_messages,
                                    last_assistant_message: last_agent_message.clone(),
                                },
                            },
                        })
                        .await;

                    let mut abort_message = None;
                    for hook_outcome in hook_outcomes {
                        let hook_name = hook_outcome.hook_name;
                        match hook_outcome.result {
                            HookResult::Success => {}
                            HookResult::FailedContinue(error) => {
                                warn!(
                                    turn_id = %turn_context.sub_id,
                                    hook_name = %hook_name,
                                    error = %error,
                                    "after_agent hook failed; continuing"
                                );
                            }
                            HookResult::FailedAbort(error) => {
                                let message = format!(
                                    "after_agent hook '{hook_name}' failed and aborted turn completion: {error}"
                                );
                                warn!(
                                    turn_id = %turn_context.sub_id,
                                    hook_name = %hook_name,
                                    error = %error,
                                    "after_agent hook failed; aborting operation"
                                );
                                if abort_message.is_none() {
                                    abort_message = Some(message);
                                }
                            }
                        }
                    }
                    if let Some(message) = abort_message {
                        sess.send_event(
                            &turn_context,
                            EventMsg::Error(crate::protocol::ErrorEvent {
                                message,
                                chaos_error_info: None,
                            }),
                        )
                        .await;
                        return None;
                    }
                    break;
                }
                continue;
            }
            Err(ChaosErr::TurnAborted) => {
                // Aborted turn is reported via a different event.
                break;
            }
            Err(ChaosErr::InvalidImageRequest()) => {
                let mut state = sess.state.lock().await;
                crate::util::error_or_panic(
                    "Invalid image detected; sanitizing tool output to prevent poisoning",
                );
                if state.history.replace_last_turn_images("Invalid image") {
                    continue;
                }
                let event = EventMsg::Error(crate::protocol::ErrorEvent {
                    message: "Invalid image in your last message. Please remove it and try again."
                        .to_string(),
                    chaos_error_info: Some(chaos_ipc::protocol::ChaosErrorInfo::BadRequest),
                });
                sess.send_event(&turn_context, event).await;
                break;
            }
            Err(e) => {
                info!("Turn error: {e:#}");
                let event = EventMsg::Error(e.to_error_event(/*message_prefix*/ None));
                sess.send_event(&turn_context, event).await;
                // let the user continue the conversation
                break;
            }
        }
    }

    last_agent_message
}

pub(crate) async fn built_tools(
    sess: &Session,
    turn_context: &TurnContext,
    _input: &[ResponseItem],
    cancellation_token: &CancellationToken,
) -> ChaosResult<Arc<ToolRouter>> {
    let mcp_connection_manager = sess.services.mcp_connection_manager.read().await;
    let has_mcp_servers = mcp_connection_manager.has_servers();
    let mcp_tools = mcp_connection_manager
        .list_all_tools()
        .or_cancel(cancellation_token)
        .await?;
    drop(mcp_connection_manager);

    // Read static module tools from the catalog.
    let mut catalog_tools = {
        let catalog = sess
            .services
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        catalog
            .tools()
            .iter()
            .filter(|(s, _)| matches!(s, crate::catalog::CatalogSource::Module(_)))
            .map(|(s, t)| {
                let source_name = match s {
                    crate::catalog::CatalogSource::Module(name) => name.clone(),
                    crate::catalog::CatalogSource::Mcp(name) => name.clone(),
                };
                (
                    source_name,
                    chaos_traits::catalog::CatalogTool {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        input_schema: t.input_schema.clone(),
                        annotations: t.annotations.clone(),
                        read_only_hint: t.read_only_hint,
                        supports_parallel_tool_calls: t.supports_parallel_tool_calls,
                    },
                )
            })
            .collect::<Vec<_>>()
    };

    // Add script tools from the hallucinate engine (Lua/WASM user scripts).
    if let Some(ref handle) = sess.services.hallucinate {
        let script_tools = handle.list_tools().await;
        for tool in script_tools {
            catalog_tools.push((
                "hallucinate".to_string(),
                chaos_traits::catalog::CatalogTool {
                    name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                    annotations: None,
                    read_only_hint: None,
                    supports_parallel_tool_calls: true,
                },
            ));
        }
    }

    let plan_mode = turn_context.collaboration_mode.mode == ModeKind::Plan;
    Ok(Arc::new(ToolRouter::from_config(
        &turn_context.tools_config,
        ToolRouterParams {
            mcp_tools: has_mcp_servers.then(|| {
                mcp_tools
                    .into_iter()
                    .map(|(name, tool)| (name, tool.tool))
                    .collect()
            }),
            app_tools: None,
            dynamic_tools: turn_context.dynamic_tools.as_slice(),
            catalog_tools,
            hallucinate: sess.services.hallucinate.clone(),
            plan_mode,
        },
    )))
}
