use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::AgentMessageContentDeltaEvent;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExitedReviewModeEvent;
use chaos_ipc::protocol::ItemCompletedEvent;
use chaos_ipc::protocol::ReviewOutputEvent;
use chaos_ipc::protocol::SubAgentSource;
use tokio_util::sync::CancellationToken;

use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::chaos_delegate::run_chaos_process_one_shot;
use crate::config::Constrained;
use crate::features::Feature;
use crate::review_format::format_review_findings_block;
use crate::review_format::render_review_output_text;
use crate::state::TaskKind;
use chaos_ipc::user_input::UserInput;

use super::SessionTask;
use super::SessionTaskContext;

#[derive(Clone, Copy)]
pub(crate) struct ReviewTask;

impl ReviewTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl SessionTask for ReviewTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Review
    }

    fn span_name(&self) -> &'static str {
        "session_task.review"
    }

    fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
        Box::pin(async move {
            session.session.services.session_telemetry.counter(
                "chaos.task.review",
                /*inc*/ 1,
                &[],
            );

            let output = match start_review_conversation(
                session.clone(),
                ctx.clone(),
                input,
                cancellation_token.clone(),
            )
            .await
            {
                Some(receiver) => {
                    process_review_events(session.clone(), ctx.clone(), receiver).await
                }
                None => None,
            };
            if !cancellation_token.is_cancelled() {
                exit_review_mode(session.clone_session(), output.clone(), ctx.clone()).await;
            }
            None
        })
    }

    fn abort(
        &self,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            exit_review_mode(session.clone_session(), /*review_output*/ None, ctx).await;
        })
    }
}

async fn start_review_conversation(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    input: Vec<UserInput>,
    cancellation_token: CancellationToken,
) -> Option<async_channel::Receiver<Event>> {
    let config = ctx.config.clone();
    let mut sub_agent_config = config.as_ref().clone();
    // Carry over review-only feature restrictions so the delegate cannot
    // re-enable blocked tools (web search, collab tools, view image).
    if let Err(err) = sub_agent_config
        .web_search_mode
        .set(WebSearchMode::Disabled)
    {
        panic!("by construction Constrained<WebSearchMode> must always support Disabled: {err}");
    }
    let _ = sub_agent_config.features.disable(Feature::SpawnCsv);
    sub_agent_config.collab_enabled = false;

    // Set explicit review rubric for the sub-agent.  If a reviewer persona is
    // active its instructions lead the system prompt so the model adopts the
    // persona before reading the rubric.
    let base = if let Some(persona_instructions) = sub_agent_config.minion_instructions.take() {
        format!("{}\n\n{}", persona_instructions, crate::REVIEW_PROMPT)
    } else {
        crate::REVIEW_PROMPT.to_string()
    };
    sub_agent_config.base_instructions = Some(base);
    sub_agent_config.permissions.approval_policy =
        Constrained::allow_only(ApprovalPolicy::Headless);

    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| ctx.model_info.slug.clone());
    sub_agent_config.model = Some(model);
    // Anthropic Messages API rejects requests with output_schema; fall back to
    // the prompt-embedded JSON contract (RESPONSE FORMAT in review_prompt.md).
    let output_schema =
        if crate::model_provider_info::is_anthropic_wire(ctx.provider.base_url.as_deref()) {
            None
        } else {
            Some(review_output_schema())
        };
    (run_chaos_process_one_shot(
        sub_agent_config,
        session.auth_manager(),
        session.models_manager(),
        input,
        session.clone_session(),
        ctx.clone(),
        cancellation_token,
        SubAgentSource::Review,
        output_schema,
        /*initial_history*/ None,
    )
    .await)
        .ok()
        .map(|io| io.rx_event)
}

async fn process_review_events(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    receiver: async_channel::Receiver<Event>,
) -> Option<ReviewOutputEvent> {
    let mut prev_agent_message: Option<Event> = None;
    while let Ok(event) = receiver.recv().await {
        match event.clone().msg {
            EventMsg::AgentMessage(_) => {
                if let Some(prev) = prev_agent_message.take() {
                    session
                        .clone_session()
                        .send_event(ctx.as_ref(), prev.msg)
                        .await;
                }
                prev_agent_message = Some(event);
            }
            // Suppress ItemCompleted and streaming deltas for assistant messages
            // during review: this flow uses structured output exclusively.
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(_),
                ..
            })
            | EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent { .. }) => {}
            EventMsg::TurnComplete(task_complete) => {
                // Parse review output from the last agent message (if present).
                let out = task_complete
                    .last_agent_message
                    .as_deref()
                    .map(parse_review_output_event);
                return out;
            }
            EventMsg::TurnAborted(_) => {
                // Cancellation or abort: consumer will finalize with None.
                return None;
            }
            other => {
                session
                    .clone_session()
                    .send_event(ctx.as_ref(), other)
                    .await;
            }
        }
    }
    // Channel closed without TurnComplete: treat as interrupted.
    None
}

/// Parse a ReviewOutputEvent from a text blob returned by the reviewer model.
/// If the text is valid JSON matching ReviewOutputEvent, deserialize it.
/// Otherwise, attempt to extract the first JSON object substring and parse it.
/// If parsing still fails, return a structured fallback carrying the plain text
/// in `overall_explanation`.
fn parse_review_output_event(text: &str) -> ReviewOutputEvent {
    if let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(text) {
        return ev;
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}'))
        && start < end
        && let Some(slice) = text.get(start..=end)
        && let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(slice)
    {
        return ev;
    }
    ReviewOutputEvent {
        overall_explanation: text.to_string(),
        ..Default::default()
    }
}

/// Emits an ExitedReviewMode Event with optional ReviewOutput,
/// and records a developer message with the review output.
pub(crate) async fn exit_review_mode(
    session: Arc<Session>,
    review_output: Option<ReviewOutputEvent>,
    ctx: Arc<TurnContext>,
) {
    const REVIEW_USER_MESSAGE_ID: &str = "review_rollout_user";
    const REVIEW_ASSISTANT_MESSAGE_ID: &str = "review_rollout_assistant";
    let (user_message, assistant_message) = if let Some(out) = review_output.clone() {
        let mut findings_str = String::new();
        let text = out.overall_explanation.trim();
        if !text.is_empty() {
            findings_str.push_str(text);
        }
        if !out.findings.is_empty() {
            let block = format_review_findings_block(&out.findings, /*selection*/ None);
            findings_str.push_str(&format!("\n{block}"));
        }
        let rendered =
            crate::client_common::REVIEW_EXIT_SUCCESS_TMPL.replace("{results}", &findings_str);
        let assistant_message = render_review_output_text(&out);
        (rendered, assistant_message)
    } else {
        let rendered = crate::client_common::REVIEW_EXIT_INTERRUPTED_TMPL.to_string();
        let assistant_message =
            "Review was interrupted. Please re-run /review and wait for it to complete."
                .to_string();
        (rendered, assistant_message)
    };

    session
        .record_conversation_items(
            &ctx,
            &[ResponseItem::Message {
                id: Some(REVIEW_USER_MESSAGE_ID.to_string()),
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text: user_message }],
                end_turn: None,
                phase: None,
            }],
        )
        .await;

    session
        .send_event(
            ctx.as_ref(),
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent { review_output }),
        )
        .await;
    session
        .record_response_item_and_emit_turn_item(
            ctx.as_ref(),
            ResponseItem::Message {
                id: Some(REVIEW_ASSISTANT_MESSAGE_ID.to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: assistant_message,
                }],
                end_turn: None,
                phase: None,
            },
        )
        .await;

    // Review turns can run before any regular user turn, so explicitly
    // materialize persisted session state. Do this after emitting review output
    // so journal/bootstrap work cannot delay client-facing items.
    session.ensure_rollout_materialized().await;
}

fn review_output_schema() -> serde_json::Value {
    let mut schema = mcp_host::macros::schema_for::<chaos_ipc::protocol::ReviewOutputEvent>();
    strip_unsupported_schema_keywords(&mut schema);
    schema
}

/// Strip keywords that strict structured-output providers reject.
/// schemars emits `format` on numeric/integer types and `minimum: 0` on
/// unsigned integers — neither is accepted by OpenAI's strict JSON Schema path.
fn strip_unsupported_schema_keywords(value: &mut serde_json::Value) {
    if let Some(map) = value.as_object_mut() {
        map.remove("format");
        map.remove("minimum");
        map.remove("maximum");
        map.remove("exclusiveMinimum");
        map.remove("exclusiveMaximum");
        map.remove("$schema");
        for v in map.values_mut() {
            strip_unsupported_schema_keywords(v);
        }
    } else if let Some(arr) = value.as_array_mut() {
        for v in arr.iter_mut() {
            strip_unsupported_schema_keywords(v);
        }
    }
}
