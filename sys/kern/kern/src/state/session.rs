//! Session-wide mutable state.

use chaos_ipc::models::PermissionProfile;
use chaos_ipc::models::ResponseItem;
use std::collections::HashMap;

use crate::chaos::PreviousTurnSettings;
use crate::chaos::SessionConfiguration;
use crate::chaos::title_reflex::SessionTitleReflex;
use crate::context_manager::ContextManager;
use crate::models_manager::observations::ContextWindows;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use crate::sandboxing::merge_permission_profiles;
use crate::truncate::TruncationPolicy;
use chaos_context::pressure::Window;
use chaos_ipc::protocol::RateLimitWindow;
use chaos_ipc::protocol::TurnContextItem;

/// Persistent, session-scoped state owned by the session runner task.
pub(crate) struct SessionState {
    pub(crate) session_configuration: SessionConfiguration,
    pub(crate) history: ContextManager,
    pub(crate) latest_rate_limits: Option<RateLimitSnapshot>,
    /// The last merged snapshot of every provider's limits, keyed by provider
    /// and limit, so a partial update keeps its own windows even after another
    /// provider's limits came in between, and never borrows another's.
    rate_limits_by_limit: HashMap<(String, String), RateLimitSnapshot>,
    /// The provider identity behind `latest_rate_limits`.
    latest_rate_limits_provider: Option<String>,
    pub(crate) server_reasoning_included: bool,
    pub(crate) dependency_env: HashMap<String, String>,
    /// Settings used by the latest regular user turn, used for turn-to-turn
    /// model/realtime handling on subsequent regular turns (including full-context
    /// reinjection after resume or `/compact`).
    previous_turn_settings: Option<PreviousTurnSettings>,
    pub(crate) pending_session_start_source: Option<chaos_dtrace::SessionStartSource>,
    granted_permissions: Option<PermissionProfile>,
    /// Context-pressure window state; rotates on each distillation.
    pub(crate) pressure: Window,
    pub(crate) session_title_reflex: SessionTitleReflex,
    pub(crate) machine_recovery: crate::machine_recovery::Recovery,
    pub(crate) goal: crate::goal::GoalGate,
    /// Journaled observations are session-local even when model caches are shared.
    pub(crate) recorded_context_windows: ContextWindows,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new(session_configuration: SessionConfiguration) -> Self {
        let history = ContextManager::new();
        Self {
            session_configuration,
            history,
            latest_rate_limits: None,
            rate_limits_by_limit: HashMap::new(),
            latest_rate_limits_provider: None,
            server_reasoning_included: false,
            dependency_env: HashMap::new(),
            previous_turn_settings: None,
            pending_session_start_source: None,
            granted_permissions: None,
            pressure: Window::new(),
            session_title_reflex: SessionTitleReflex::default(),
            machine_recovery: Default::default(),
            goal: Default::default(),
            recorded_context_windows: Default::default(),
        }
    }

    // History helpers
    pub(crate) fn record_items<I>(&mut self, items: I, policy: TruncationPolicy)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        let goal = &mut self.goal;
        self.history
            .record_items(items.into_iter().inspect(|item| goal.record(item)), policy);
    }

    pub(crate) fn previous_turn_settings(&self) -> Option<PreviousTurnSettings> {
        self.previous_turn_settings.clone()
    }
    pub(crate) fn set_previous_turn_settings(
        &mut self,
        previous_turn_settings: Option<PreviousTurnSettings>,
    ) {
        self.previous_turn_settings = previous_turn_settings;
    }

    pub(crate) fn clone_history(&self) -> ContextManager {
        self.history.clone()
    }

    pub(crate) fn replace_history(
        &mut self,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) {
        self.history.replace(items);
        self.history
            .set_reference_context_item(reference_context_item);
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        self.history.set_token_info(info);
    }

    pub(crate) fn set_reference_context_item(&mut self, item: Option<TurnContextItem>) {
        self.history.set_reference_context_item(item);
    }

    pub(crate) fn reference_context_item(&self) -> Option<TurnContextItem> {
        self.history.reference_context_item()
    }

    // Token/rate limit helpers
    pub(crate) fn update_token_info_from_usage(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<i64>,
    ) {
        self.pressure.observe_server_baseline(usage.input_tokens);
        self.history.update_token_info(usage, model_context_window);
    }

    pub(crate) fn token_info(&self) -> Option<TokenUsageInfo> {
        self.history.token_info()
    }

    /// Record a snapshot from `provider`, an identity string for the provider
    /// that sent it.
    pub(crate) fn set_rate_limits(&mut self, provider: &str, snapshot: RateLimitSnapshot) {
        let key = (
            provider.to_string(),
            snapshot
                .limit_id
                .clone()
                .unwrap_or_else(|| "chaos".to_string()),
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
            });
        let latest_same_provider = self
            .latest_rate_limits
            .as_ref()
            .filter(|_| self.latest_rate_limits_provider.as_deref() == Some(provider));
        let merged = merge_rate_limit_fields(
            latest_same_provider,
            self.rate_limits_by_limit.get(&key),
            snapshot,
            now,
        );
        self.rate_limits_by_limit.insert(key, merged.clone());
        self.latest_rate_limits = Some(merged);
        self.latest_rate_limits_provider = Some(provider.to_string());
    }

    pub(crate) fn token_info_and_rate_limits(
        &self,
    ) -> (Option<TokenUsageInfo>, Option<RateLimitSnapshot>) {
        (self.token_info(), self.latest_rate_limits.clone())
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: i64) {
        self.history.set_token_usage_full(context_window);
    }

    pub(crate) fn get_total_token_usage(&self, server_reasoning_included: bool) -> i64 {
        self.history
            .get_total_token_usage(server_reasoning_included)
    }

    pub(crate) fn set_server_reasoning_included(&mut self, included: bool) {
        self.server_reasoning_included = included;
    }

    pub(crate) fn server_reasoning_included(&self) -> bool {
        self.server_reasoning_included
    }

    pub(crate) fn dependency_env(&self) -> HashMap<String, String> {
        self.dependency_env.clone()
    }

    pub(crate) fn set_pending_session_start_source(
        &mut self,
        value: Option<chaos_dtrace::SessionStartSource>,
    ) {
        self.pending_session_start_source = value;
    }

    pub(crate) fn take_pending_session_start_source(
        &mut self,
    ) -> Option<chaos_dtrace::SessionStartSource> {
        self.pending_session_start_source.take()
    }

    pub(crate) fn record_granted_permissions(&mut self, permissions: PermissionProfile) {
        self.granted_permissions =
            merge_permission_profiles(self.granted_permissions.as_ref(), Some(&permissions));
    }

    pub(crate) fn granted_permissions(&self) -> Option<PermissionProfile> {
        self.granted_permissions.clone()
    }
}

// A snapshot fills only what it reports. Windows it leaves out are kept from
// the last snapshot of the same limit, until they reset (`now` is Unix seconds).
// Credits and plan information belong to the account, so they are kept from
// `latest` (the latest snapshot, already limited to the same provider) when it
// is of the same account family (`chaos`, `chaos_other`). For `limit_id`, treat
// missing values as the default `"chaos"` bucket.
fn merge_rate_limit_fields(
    latest: Option<&RateLimitSnapshot>,
    same_limit: Option<&RateLimitSnapshot>,
    mut snapshot: RateLimitSnapshot,
    now: i64,
) -> RateLimitSnapshot {
    let current = |window: &Option<RateLimitWindow>| {
        window
            .clone()
            .filter(|window| window.resets_at.is_none_or(|resets_at| resets_at > now))
    };
    let limit_id = snapshot
        .limit_id
        .get_or_insert_with(|| "chaos".to_string())
        .clone();
    let same_account = latest.filter(|prior| {
        let prior_id = prior.limit_id.as_deref().unwrap_or("chaos");
        account_family(prior_id) == account_family(&limit_id)
    });
    if snapshot.primary.is_none() {
        snapshot.primary = same_limit.and_then(|prior| current(&prior.primary));
    }
    if snapshot.secondary.is_none() {
        snapshot.secondary = same_limit.and_then(|prior| current(&prior.secondary));
    }
    if snapshot.credits.is_none() {
        snapshot.credits = same_account.and_then(|prior| prior.credits.clone());
    }
    if snapshot.plan_type.is_none() {
        snapshot.plan_type = same_account.and_then(|prior| prior.plan_type);
    }
    snapshot
}

fn account_family(limit_id: &str) -> &str {
    limit_id.split('_').next().unwrap_or(limit_id)
}

#[cfg(test)]
mod tests;
