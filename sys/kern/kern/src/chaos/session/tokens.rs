use std::collections::HashMap;

use chaos_ipc::models::BaseInstructions;
use chaos_ipc::protocol::TokenCountEvent;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::TokenUsageInfo;

use crate::context_manager::TotalTokenUsageBreakdown;

use super::Session;
use crate::chaos::TurnContext;

impl Session {
    pub(crate) async fn get_total_token_usage(&self) -> i64 {
        let state = self.state.lock().await;
        state.get_total_token_usage(state.server_reasoning_included())
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
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: estimated_total_tokens.max(0),
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
            let result = throttle_machines::gcra::check(tat, now, emission_interval, 0.0);
            RATE_TATS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), result.new_tat);
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
        let event =
            chaos_ipc::protocol::EventMsg::TokenCount(TokenCountEvent { info, rate_limits });
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
