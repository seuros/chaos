use std::collections::HashMap;

use chaos_ipc::models::BaseInstructions;
use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::TokenCountEvent;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::TokenUsageInfo;

use crate::context_manager::TotalTokenUsageBreakdown;

use super::Session;
use crate::chaos::TurnContext;

pub(crate) const DISTILLATION_RESERVE_TOKENS: i64 = 20_000;

fn deferral_ceiling_for_windows(
    raw_context_window: i64,
    effective_context_window: i64,
) -> Option<i64> {
    let raw_safety_ceiling = raw_context_window.saturating_mul(9).saturating_div(10);
    let distillation_ceiling = effective_context_window.saturating_sub(DISTILLATION_RESERVE_TOKENS);
    Some(raw_safety_ceiling.min(distillation_ceiling)).filter(|ceiling| *ceiling > 0)
}

pub(crate) fn compaction_deferral_ceiling(turn_context: &TurnContext) -> Option<i64> {
    let raw_context_window = turn_context.model_info.context_window?;
    let effective_context_window = turn_context.model_context_window()?;
    deferral_ceiling_for_windows(raw_context_window, effective_context_window)
}

fn compaction_reflex_reserve(context_window: i64, compaction_token_limit: i64) -> i64 {
    let soft_to_hard_gap = context_window.saturating_sub(compaction_token_limit).max(1);
    let proportional_lead = compaction_token_limit.saturating_div(4).max(1);
    soft_to_hard_gap.min(proportional_lead)
}

#[cfg(test)]
mod compaction_control_tests {
    use super::deferral_ceiling_for_windows;

    #[test]
    fn observed_400k_window_keeps_distillation_reserve() {
        assert_eq!(
            deferral_ceiling_for_windows(400_000, 380_000),
            Some(360_000)
        );
    }

    #[test]
    fn raw_window_safety_fraction_can_be_the_binding_ceiling() {
        assert_eq!(
            deferral_ceiling_for_windows(300_000, 295_000),
            Some(270_000)
        );
    }
}

fn compaction_reflex_due(remaining: i64, context_window: i64, compaction_token_limit: i64) -> bool {
    remaining <= compaction_reflex_reserve(context_window, compaction_token_limit)
}

fn compaction_reflex_follow_up_allowed(active_tokens: i64, deferral_ceiling: i64) -> bool {
    active_tokens < deferral_ceiling
}

#[allow(clippy::too_many_arguments)]
fn compaction_reflex_instructions(
    window_id: &str,
    window_number: i64,
    active_tokens: i64,
    tokens_until_compaction: i64,
    compaction_token_limit: i64,
    context_window: i64,
    bounded_control: bool,
    title_review_guidance: Option<&str>,
) -> String {
    let control_guidance = if bounded_control {
        format!(
            "You have bounded timing control for this pressure window. If you need a little more room after continuity work, call `compaction_control` with action `defer_once` and window_id `{window_id}`. You may instead request `compact_now`. Doing neither means normal automatic compaction will proceed. A deferral is one-time and cannot override Chaos's fixed safety ceiling.\n\n"
        )
    } else {
        String::new()
    };
    let title_review_guidance = title_review_guidance
        .map(|guidance| format!("{guidance}\n\n"))
        .unwrap_or_default();
    format!(
        "<compaction_reflex window_id=\"{window_id}\" window_number=\"{window_number}\">\n\
Automatic context compaction is approaching. This notice is for you, the continuing agent, not merely for the user.\n\n\
Current context: approximately {active_tokens} active tokens.\n\
Automatic compaction threshold: {compaction_token_limit} tokens.\n\
Effective input window: {context_window} tokens.\n\
Estimated tokens remaining before automatic compaction: {tokens_until_compaction}.\n\n\
Before continuing substantive work, pause and consider whether anything should be preserved across compaction. Use your normal tools now to perform any continuity practices required by your existing instructions: for example, recording current commitments or operational state, updating memory, or writing a personal journal entry when there is genuine narrative shape. Do not manufacture memories or journal entries when your instructions say no action is warranted. Do not substitute a generic user-facing summary for the practices available to you.\n\n\
{title_review_guidance}\
{control_guidance}\
After completing or deliberately declining those actions, continue the current turn normally.\n\
</compaction_reflex>"
    )
}

impl Session {
    /// Measures the current context load against the model's allotments,
    /// honoring the configured scope and the current pressure-window baseline.
    pub(crate) async fn allotment_status(
        &self,
        turn_context: &TurnContext,
    ) -> chaos_context::allotment::AllotmentStatus {
        let mut state = self.state.lock().await;
        let active_tokens = state.get_total_token_usage(state.server_reasoning_included());
        let baseline = state
            .pressure
            .baseline()
            .map(chaos_context::pressure::Baseline::tokens);
        let effective_context_window = turn_context.model_context_window();
        let control_is_current = match state.pressure.control() {
            chaos_context::pressure::Control::Deferred(deferral) => {
                deferral.model == turn_context.model_info.slug
                    && Some(deferral.effective_context_window) == effective_context_window
                    && Some(deferral.ceiling) == compaction_deferral_ceiling(turn_context)
            }
            chaos_context::pressure::Control::CompactRequested(request) => {
                request.model == turn_context.model_info.slug
                    && Some(request.effective_context_window) == effective_context_window
            }
            chaos_context::pressure::Control::Normal => true,
        };
        if !control_is_current {
            state
                .pressure
                .restore_control(chaos_context::pressure::Control::Normal);
        }
        let deferred = matches!(
            state.pressure.control(),
            chaos_context::pressure::Control::Deferred(_)
        );
        chaos_context::allotment::status(
            turn_context.config.model_auto_compact_token_limit_scope,
            active_tokens,
            baseline,
            chaos_context::allotment::Limits {
                auto_distill_token_limit: turn_context.model_info.auto_compact_token_limit(),
                context_window: effective_context_window,
                deferral_ceiling: compaction_deferral_ceiling(turn_context),
            },
            deferred,
        )
    }

    /// Inject a model-visible, tool-compatible continuity reflex at most once
    /// per pressure window when the session enters the reserve band before
    /// automatic compaction.
    pub(crate) async fn maybe_inject_compaction_reflex(
        &self,
        turn_context: &TurnContext,
    ) -> (chaos_context::allotment::AllotmentStatus, bool, bool) {
        let (allotment, instructions, follow_up_allowed) = {
            let mut state = self.state.lock().await;
            let active_tokens = state.get_total_token_usage(state.server_reasoning_included());
            let baseline = state
                .pressure
                .baseline()
                .map(chaos_context::pressure::Baseline::tokens);
            let compaction_token_limit = turn_context.model_info.auto_compact_token_limit();
            let context_window = turn_context.model_context_window();
            let deferral_ceiling = compaction_deferral_ceiling(turn_context);
            let deferred = matches!(
                state.pressure.control(),
                chaos_context::pressure::Control::Deferred(deferral)
                    if deferral.model == turn_context.model_info.slug
                        && Some(deferral.effective_context_window) == context_window
                        && Some(deferral.ceiling) == deferral_ceiling
            );
            let allotment = chaos_context::allotment::status(
                turn_context.config.model_auto_compact_token_limit_scope,
                active_tokens,
                baseline,
                chaos_context::allotment::Limits {
                    auto_distill_token_limit: compaction_token_limit,
                    context_window,
                    deferral_ceiling,
                },
                deferred,
            );

            let instructions = match (
                compaction_token_limit,
                context_window,
                allotment.tokens_until_distillation,
            ) {
                (Some(compaction_token_limit), Some(context_window), Some(remaining))
                    if compaction_reflex_due(remaining, context_window, compaction_token_limit)
                        && deferral_ceiling.is_some_and(|ceiling| active_tokens < ceiling)
                        && state.pressure.claim_reminder() =>
                {
                    let title_review_guidance = if super::super::title_reflex::title_review_enabled(
                        turn_context.config.terminal_title,
                        &state.session_configuration.session_source,
                    ) {
                        let process_name = state.session_configuration.process_name.clone();
                        state
                            .session_title_reflex
                            .claim_for_compaction(active_tokens)
                            .map(|trigger| {
                                super::super::title_reflex::title_review_instructions(
                                    process_name.as_deref(),
                                    trigger,
                                )
                            })
                    } else {
                        None
                    };
                    Some(compaction_reflex_instructions(
                        &state.pressure.window_id().to_string(),
                        i64::try_from(state.pressure.window_number()).unwrap_or(i64::MAX),
                        active_tokens,
                        remaining,
                        compaction_token_limit,
                        context_window,
                        matches!(
                            turn_context.config.agent_compaction_control,
                            crate::config::AgentCompactionControl::Bounded
                        ),
                        title_review_guidance.as_deref(),
                    ))
                }
                _ => None,
            };
            let follow_up_allowed = instructions.is_some()
                && deferral_ceiling.is_some_and(|ceiling| {
                    compaction_reflex_follow_up_allowed(active_tokens, ceiling)
                });
            (allotment, instructions, follow_up_allowed)
        };

        let injected = if let Some(instructions) = instructions {
            let item: ResponseItem = DeveloperInstructions::new(instructions).into();
            self.record_conversation_items(turn_context, &[item]).await;
            self.services
                .session_telemetry
                .counter("chaos.compaction.control_offered", 1, &[]);
            true
        } else {
            false
        };
        (allotment, injected, follow_up_allowed)
    }

    pub(crate) async fn compaction_requested(&self, turn_context: &TurnContext) -> bool {
        let mut state = self.state.lock().await;
        let effective_context_window = turn_context.model_context_window();
        match state.pressure.control() {
            chaos_context::pressure::Control::CompactRequested(request)
                if request.model == turn_context.model_info.slug
                    && Some(request.effective_context_window) == effective_context_window =>
            {
                true
            }
            chaos_context::pressure::Control::CompactRequested(_) => {
                state.pressure.clear_compaction_request();
                false
            }
            _ => false,
        }
    }

    pub(crate) async fn clear_compaction_request(&self) {
        self.state.lock().await.pressure.clear_compaction_request();
    }

    pub(crate) async fn get_total_token_usage_breakdown(&self) -> TotalTokenUsageBreakdown {
        let state = self.state.lock().await;
        state.history.get_total_token_usage_breakdown()
    }

    pub(crate) async fn total_token_usage(&self) -> Option<TokenUsage> {
        let state = self.state.lock().await;
        state.token_info().map(|info| info.total_token_usage)
    }

    pub(crate) async fn get_estimated_token_count(
        &self,
        turn_context: &TurnContext,
    ) -> Option<i64> {
        let state = self.state.lock().await;
        state.history.estimate_token_count(turn_context)
    }

    pub(crate) async fn get_base_instructions(&self) -> BaseInstructions {
        let state = self.state.lock().await;
        BaseInstructions {
            text: state.session_configuration.base_instructions.clone(),
        }
    }

    pub(crate) async fn update_token_usage_info(
        &self,
        turn_context: &TurnContext,
        token_usage: Option<&crate::protocol::TokenUsage>,
    ) {
        if let Some(token_usage) = token_usage {
            let mut state = self.state.lock().await;
            state.update_token_info_from_usage(token_usage, turn_context.model_context_window());
        }
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn recompute_token_usage(&self, turn_context: &TurnContext) {
        let history = self.clone_history().await;
        let base_instructions = self.get_base_instructions().await;
        let Some(estimated_total_tokens) =
            history.estimate_token_count_with_base_instructions(&base_instructions)
        else {
            return;
        };
        {
            let mut state = self.state.lock().await;
            let mut info = state.token_info().unwrap_or(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: None,
            });

            info.last_token_usage = TokenUsage {
                input_tokens: 0,
                cache_creation_input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: estimated_total_tokens.max(0),
                provider_request_count: 0,
            };

            if let Some(model_context_window) = turn_context.model_context_window() {
                info.model_context_window = Some(model_context_window);
            }

            state.set_token_info(Some(info));
        }
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn update_rate_limits(
        &self,
        turn_context: &TurnContext,
        new_rate_limits: crate::protocol::RateLimitSnapshot,
    ) {
        if let Some(ref id) = new_rate_limits.limit_id {
            use std::sync::LazyLock;
            use std::sync::Mutex;
            static RATE_TATS: LazyLock<Mutex<HashMap<String, f64>>> =
                LazyLock::new(|| Mutex::new(HashMap::new()));

            let now = jiff::Timestamp::now().as_second() as f64;
            let emission_interval = 1.0_f64;
            let tat = RATE_TATS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(id)
                .copied()
                .unwrap_or(0.0);
            let result = {
                use throttle_machines::gate::Gate;
                throttle_machines::gcra::Gcra::check(
                    tat,
                    now,
                    throttle_machines::gcra::GcraParams {
                        emission_interval,
                        delay_tolerance: 0.0,
                    },
                )
            };
            RATE_TATS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), result.state);
            if !result.allowed {
                tracing::warn!(
                    limit_id = %id,
                    retry_after = result.retry_after,
                    "rate limit snapshot arriving faster than 1 Hz"
                );
            }
        }

        {
            let mut state = self.state.lock().await;
            state.set_rate_limits(new_rate_limits);
        }
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn set_server_reasoning_included(&self, included: bool) {
        let mut state = self.state.lock().await;
        state.set_server_reasoning_included(included);
    }

    pub(super) async fn send_token_count_event(&self, turn_context: &TurnContext) {
        let (info, rate_limits) = {
            let state = self.state.lock().await;
            state.token_info_and_rate_limits()
        };
        let event = chaos_ipc::protocol::EventMsg::TokenCount(TokenCountEvent {
            info,
            rate_limits,
            provider_request_started: false,
        });
        self.send_event(turn_context, event).await;
    }

    /// Record one model-provider request dispatch for this turn.
    ///
    /// Call this immediately before invoking the provider client. It therefore
    /// counts tool continuations, lifecycle-hook continuations, and dispatched
    /// requests that fail while opening or consuming the response stream.
    pub(crate) async fn record_provider_request_started(&self, turn_context: &TurnContext) {
        let provider_request_count = {
            let mut state = self.state.lock().await;
            let mut info = state.token_info().unwrap_or(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: turn_context.model_context_window(),
            });
            let provider_request_count = info.record_provider_request();
            state.set_token_info(Some(info));
            provider_request_count
        };
        tracing::debug!(
            process_id = %self.conversation_id,
            turn_id = %turn_context.sub_id,
            provider = %turn_context.provider.name,
            model = %turn_context.model_info.slug,
            provider_request_count,
            "provider request dispatched"
        );
        let (info, rate_limits) = {
            let state = self.state.lock().await;
            state.token_info_and_rate_limits()
        };
        let event = chaos_ipc::protocol::EventMsg::TokenCount(TokenCountEvent {
            info,
            rate_limits,
            provider_request_started: true,
        });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn set_total_tokens_full(&self, turn_context: &TurnContext) {
        if let Some(context_window) = turn_context.model_context_window() {
            let mut state = self.state.lock().await;
            state.set_token_usage_full(context_window);
        }
        self.send_token_count_event(turn_context).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_reflex_uses_the_soft_to_hard_limit_gap() {
        assert_eq!(compaction_reflex_reserve(400_000, 350_000), 50_000);
        assert!(!compaction_reflex_due(50_001, 400_000, 350_000));
        assert!(compaction_reflex_due(50_000, 400_000, 350_000));
    }

    #[test]
    fn compaction_reflex_keeps_a_minimum_reserve() {
        assert_eq!(compaction_reflex_reserve(100, 100), 1);
        assert!(compaction_reflex_due(1, 100, 100));
    }

    #[test]
    fn compaction_reflex_does_not_fire_immediately_for_a_low_soft_limit() {
        assert_eq!(compaction_reflex_reserve(400_000, 100_000), 25_000);
        assert!(!compaction_reflex_due(100_000, 400_000, 100_000));
        assert!(compaction_reflex_due(25_000, 400_000, 100_000));
    }

    #[test]
    fn compaction_reflex_follow_up_stays_below_the_fixed_ceiling() {
        assert!(compaction_reflex_follow_up_allowed(359_999, 360_000));
        assert!(!compaction_reflex_follow_up_allowed(360_000, 360_000));
        assert!(!compaction_reflex_follow_up_allowed(400_000, 360_000));
    }

    #[test]
    fn compaction_reflex_addresses_the_agent_and_preserves_choice() {
        let instructions = compaction_reflex_instructions(
            "window-1", 2, 300_000, 50_000, 350_000, 400_000, true, None,
        );

        assert!(instructions.contains("for you, the continuing agent"));
        assert!(instructions.contains("Use your normal tools now"));
        assert!(instructions.contains("Do not manufacture memories"));
        assert!(instructions.contains("window_id=\"window-1\""));
        assert!(instructions.contains("window_number=\"2\""));
        assert!(instructions.contains("compaction_control"));
        assert!(instructions.contains("defer_once"));
    }

    #[test]
    fn compaction_reflex_can_include_title_review_guidance() {
        let instructions = compaction_reflex_instructions(
            "window-1",
            2,
            300_000,
            50_000,
            350_000,
            400_000,
            true,
            Some("<session_title_reflex>review title</session_title_reflex>"),
        );

        assert!(instructions.contains("<session_title_reflex>review title</session_title_reflex>"));
    }
}
