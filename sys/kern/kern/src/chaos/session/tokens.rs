use std::collections::HashMap;

use chaos_ipc::models::BaseInstructions;
use chaos_ipc::protocol::CompactionPendingEvent;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::TokenCountEvent;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::TokenUsageInfo;

use crate::context_manager::TotalTokenUsageBreakdown;

use super::Session;
use crate::chaos::TurnContext;

fn compaction_warning_reserve(context_window: i64, compaction_token_limit: i64) -> i64 {
    context_window.saturating_sub(compaction_token_limit).max(1)
}

fn compaction_warning_due(
    remaining: i64,
    context_window: i64,
    compaction_token_limit: i64,
) -> bool {
    remaining <= compaction_warning_reserve(context_window, compaction_token_limit)
}

impl Session {
    /// Measures the current context load against the model's allotments,
    /// honoring the configured scope and the current pressure-window baseline.
    pub(crate) async fn allotment_status(
        &self,
        turn_context: &TurnContext,
    ) -> chaos_context::allotment::AllotmentStatus {
        let state = self.state.lock().await;
        let active_tokens = state.get_total_token_usage(state.server_reasoning_included());
        let baseline = state
            .pressure
            .baseline()
            .map(chaos_context::pressure::Baseline::tokens);
        chaos_context::allotment::status(
            turn_context.config.model_auto_compact_token_limit_scope,
            active_tokens,
            baseline,
            chaos_context::allotment::Limits {
                auto_distill_token_limit: turn_context.model_info.auto_compact_token_limit(),
                context_window: turn_context.model_context_window(),
            },
        )
    }

    /// Emits at most one durable warning per pressure window when the session
    /// enters the reserve band immediately before automatic compaction.
    ///
    /// The reserve band is the gap between the configured automatic
    /// compaction threshold and the model's hard context window. For example,
    /// a 350k threshold in a 400k window warns with 50k tokens remaining.
    pub(crate) async fn maybe_emit_compaction_pending(
        &self,
        turn_context: &TurnContext,
    ) -> chaos_context::allotment::AllotmentStatus {
        let (allotment, pending) = {
            let mut state = self.state.lock().await;
            let active_tokens = state.get_total_token_usage(state.server_reasoning_included());
            let baseline = state
                .pressure
                .baseline()
                .map(chaos_context::pressure::Baseline::tokens);
            let auto_compact_token_limit = turn_context.model_info.auto_compact_token_limit();
            let context_window = turn_context.model_context_window();
            let allotment = chaos_context::allotment::status(
                turn_context.config.model_auto_compact_token_limit_scope,
                active_tokens,
                baseline,
                chaos_context::allotment::Limits {
                    auto_distill_token_limit: auto_compact_token_limit,
                    context_window,
                },
            );

            let pending = match (
                auto_compact_token_limit,
                context_window,
                allotment.tokens_until_distillation,
            ) {
                (Some(compaction_token_limit), Some(context_window), Some(remaining)) => {
                    if compaction_warning_due(remaining, context_window, compaction_token_limit)
                        && state.pressure.claim_reminder()
                    {
                        Some(CompactionPendingEvent {
                            window_id: state.pressure.window_id().to_string(),
                            window_number: i64::try_from(state.pressure.window_number())
                                .unwrap_or(i64::MAX),
                            active_tokens,
                            scope_tokens: allotment.scope_tokens,
                            tokens_until_compaction: remaining,
                            compaction_token_limit,
                            context_window,
                        })
                    } else {
                        None
                    }
                }
                _ => None,
            };
            (allotment, pending)
        };

        if let Some(pending) = pending {
            self.send_event(turn_context, EventMsg::CompactionPending(pending))
                .await;
        }
        allotment
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
    use super::compaction_warning_due;
    use super::compaction_warning_reserve;

    #[test]
    fn compaction_warning_uses_the_soft_to_hard_limit_gap() {
        assert_eq!(compaction_warning_reserve(400_000, 350_000), 50_000);
        assert_eq!(compaction_warning_reserve(272_000, 244_800), 27_200);
        assert_eq!(compaction_warning_reserve(272_000, 217_600), 54_400);
    }

    #[test]
    fn compaction_warning_keeps_a_minimum_reserve() {
        assert_eq!(compaction_warning_reserve(100, 100), 1);
        assert_eq!(compaction_warning_reserve(100, 110), 1);
    }

    #[test]
    fn compaction_warning_starts_at_the_reserve_boundary() {
        assert!(!compaction_warning_due(50_001, 400_000, 350_000));
        assert!(compaction_warning_due(50_000, 400_000, 350_000));
        assert!(compaction_warning_due(0, 400_000, 350_000));
    }
}
