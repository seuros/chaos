//! `ModelClient` session-state management: construction, clamp toggling, and
//! provider client state.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::Ordering;

use chaos_ipc::protocol::SessionSource;
use rama::http::HeaderMap as ApiHeaderMap;
use rama::http::HeaderValue;
use tracing::warn;

use crate::auth::ChaosAuth;
use crate::client::auth_breaker;
use crate::config::ClampSettings;
use crate::error::ChaosErr;
use crate::error::Result;
use crate::model_provider_info::ModelProviderInfo;
use crate::protocol::SubAgentSource;

use super::{CurrentClientSetup, ModelClient, ModelClientSession, ModelClientState};

impl ModelClient {
    /// Creates a new session-scoped `ModelClient`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        auth_manager: Option<Arc<crate::AuthManager>>,
        conversation_id: chaos_ipc::ProcessId,
        provider_id: String,
        provider: ModelProviderInfo,
        session_source: SessionSource,
        approval_policy: chaos_ipc::protocol::ApprovalPolicy,
        model_verbosity: Option<chaos_ipc::config_types::Verbosity>,
        enable_request_compression: bool,
        beta_features_header: Option<String>,
        initial_clamped: bool,
        clamp_settings: ClampSettings,
    ) -> Self {
        let representer = if provider.is_openai() {
            chaos_parrot::SessionRepresenter::openai()
        } else {
            chaos_parrot::SessionRepresenter::wannabe()
        };
        let auth_breaker = auth_breaker::AuthBreaker::new(&provider_id);
        let antigravity_conversations =
            clamp_settings
                .antigravity
                .conversation_dir()
                .map(|directory| {
                    chaos_clamp::AntigravityConversationStore::new(
                        directory.join(format!("{conversation_id}.json")),
                    )
                });
        Self {
            state: Arc::new(ModelClientState {
                auth_manager,
                conversation_id,
                provider_id,
                provider,
                session_source,
                approval_policy,
                model_verbosity,
                enable_request_compression,
                beta_features_header,
                resolved_wire: std::sync::OnceLock::new(),
                clamped: std::sync::atomic::AtomicBool::new(initial_clamped),
                clamp_settings,
                clamp_transport: tokio::sync::Mutex::new(None),
                antigravity_transport: tokio::sync::Mutex::new(None),
                antigravity_conversations,
                antigravity_egress: tokio::sync::Mutex::new(None),
                clamp_wiretap: tokio::sync::Mutex::new(None),
                clamp_mcp_bridge: tokio::sync::Mutex::new(None),
                session: std::sync::Mutex::new(Weak::new()),
                representer,
                auth_breaker,
            }),
        }
    }

    /// Force the auth breaker closed after credentials change (e.g. a login),
    /// so the next turn probes the fresh auth state rather than waiting out the
    /// open-circuit backoff window.
    pub(crate) fn reset_auth_breaker(&self) {
        self.state.auth_breaker.reset();
    }

    pub(crate) fn bind_session(&self, session: &Arc<crate::chaos::Session>) {
        *self
            .state
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(session);
    }

    pub(super) async fn ensure_clamp_mcp_bridge(
        &self,
    ) -> std::result::Result<(PathBuf, String), String> {
        if let Some(existing) = self
            .state
            .clamp_mcp_bridge
            .lock()
            .await
            .as_ref()
            .map(|bridge| {
                (
                    bridge.socket_path().to_path_buf(),
                    bridge.token().to_string(),
                )
            })
        {
            return Ok(existing);
        }

        let session = self
            .state
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let bridge = crate::clamp_bridge::ClampSessionBridge::spawn(session)
            .await
            .map_err(|err| format!("failed to start clamp MCP bridge: {err}"))?;
        let output = (
            bridge.socket_path().to_path_buf(),
            bridge.token().to_string(),
        );
        let mut guard = self.state.clamp_mcp_bridge.lock().await;
        if guard.is_none() {
            *guard = Some(bridge);
        }
        Ok(output)
    }

    /// Creates a fresh turn-scoped streaming session.
    pub fn new_session(&self) -> ModelClientSession {
        ModelClientSession {
            client: self.clone(),
            turn_state: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Toggle clamped mode (Claude Code subprocess as transport).
    pub async fn set_clamped(&self, clamped: bool) {
        let was_clamped = self.state.clamped.swap(clamped, Ordering::Relaxed);
        if !clamped && was_clamped {
            let transport = {
                let mut guard = self.state.clamp_transport.lock().await;
                guard.take()
            };
            self.state.antigravity_transport.lock().await.take();
            if let Some(egress) = self.state.antigravity_egress.lock().await.take() {
                egress.shutdown();
            }
            self.state.clear_antigravity_conversation();
            let bridge = {
                let mut guard = self.state.clamp_mcp_bridge.lock().await;
                guard.take()
            };
            if let Some(wiretap) = self.state.clamp_wiretap.lock().await.take() {
                wiretap.shutdown();
            }
            if let Some(transport) = transport
                && let Err(err) = transport.shutdown().await
            {
                warn!("failed to shut down clamped transport: {err}");
            }
            if let Some(bridge) = bridge
                && let Err(err) = bridge.shutdown().await
            {
                warn!("failed to shut down clamp MCP bridge: {err}");
            }
        }
    }

    /// Whether the client is in clamped mode.
    pub fn is_clamped(&self) -> bool {
        self.state.clamped.load(Ordering::Relaxed)
    }

    /// Drop the current clamped Claude Code subprocess so the next clamped turn starts fresh.
    ///
    /// This is needed after session-history rewrites (for example process rollback): Chaos updates
    /// its own in-memory history, but the persistent Claude Code subprocess also has an internal
    /// transcript. Resetting the subprocess makes the next turn send a full prompt reconstructed
    /// from Chaos history instead of appending to stale clamp-side history.
    pub async fn reset_clamped_transport(&self) {
        let transport = {
            let mut guard = self.state.clamp_transport.lock().await;
            guard.take()
        };
        self.state.antigravity_transport.lock().await.take();
        if let Some(egress) = self.state.antigravity_egress.lock().await.take() {
            egress.shutdown();
        }
        self.state.clear_antigravity_conversation();
        if let Some(wiretap) = self.state.clamp_wiretap.lock().await.take() {
            wiretap.shutdown();
        }
        if let Some(transport) = transport
            && let Err(err) = transport.shutdown().await
        {
            warn!("failed to shut down clamped transport during reset: {err}");
        }
    }

    /// Get info about the clamped Claude Code subprocess (if running).
    pub async fn clamp_info(&self) -> Option<chaos_clamp::ClampInfo> {
        let guard = self.state.clamp_transport.lock().await;
        guard.as_ref().and_then(chaos_clamp::ClampTransport::info)
    }

    /// Switch the model on the clamped Claude Code subprocess.
    pub async fn set_clamp_model(&self, model: &str) -> std::result::Result<(), String> {
        let mut guard = self.state.clamp_transport.lock().await;
        if let Some(transport) = guard.as_mut() {
            transport
                .set_model(model)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        } else {
            Err("clamp transport not running".to_string())
        }
    }

    /// Get the initialization response from the clamped subprocess.
    pub async fn clamp_init_response(&self) -> Option<serde_json::Value> {
        let guard = self.state.clamp_transport.lock().await;
        guard.as_ref().and_then(|t| t.init_response().cloned())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn build_subagent_headers(&self) -> ApiHeaderMap {
        let mut extra_headers = crate::default_client::default_headers();
        if let SessionSource::SubAgent(sub) = &self.state.session_source {
            let subagent = match sub {
                SubAgentSource::Review => "review".to_string(),
                SubAgentSource::Compact => "compact".to_string(),
                SubAgentSource::MemoryConsolidation => "memory_consolidation".to_string(),
                SubAgentSource::ProcessSpawn { .. } => "collab_spawn".to_string(),
                SubAgentSource::Other(label) => label.clone(),
            };
            if let Ok(val) = HeaderValue::from_str(&subagent) {
                extra_headers.insert("x-openai-subagent", val);
            }
        }
        extra_headers
    }

    pub(super) async fn current_client_setup(&self) -> Result<CurrentClientSetup> {
        let auth = if self.state.provider.is_self_authenticated() {
            None
        } else {
            match self.state.auth_manager.as_ref() {
                Some(manager)
                    if self.state.provider_id == crate::auth::DEFAULT_AUTH_PROVIDER_ID =>
                {
                    manager.auth().await
                }
                Some(manager) => {
                    manager
                        .fresh_auth_for_provider(&self.state.provider_id)
                        .await
                }
                None => None,
            }
        };
        let api_provider = self
            .state
            .provider
            .to_api_provider(auth.as_ref().map(ChaosAuth::auth_mode))?;
        let api_auth =
            crate::api_bridge::auth_provider_from_auth(auth.clone(), &self.state.provider)?;
        Ok(CurrentClientSetup {
            auth,
            api_provider,
            api_auth,
        })
    }

    /// Gate a turn on the per-provider auth circuit breaker before any request
    /// is built. When the breaker is open within its backoff window this
    /// rejects immediately without resolving auth; otherwise it probes the live
    /// credential state via [`current_client_setup`] (the authoritative check,
    /// covering cached logins, env keys, and bearer tokens) and records the
    /// outcome. A missing-credentials result surfaces `ProviderAuthMissing`,
    /// which the client turns into a login prompt.
    pub(super) async fn auth_preflight(&self) -> Result<()> {
        if let auth_breaker::AuthGate::RejectFastFail = self.state.auth_breaker.check() {
            return Err(crate::api_bridge::provider_auth_missing(
                &self.state.provider,
            ));
        }
        match self.current_client_setup().await {
            Ok(_) => {
                self.state.auth_breaker.record(/*authenticated*/ true);
                Ok(())
            }
            Err(err @ ChaosErr::ProviderAuthMissing(_)) => {
                self.state.auth_breaker.record(/*authenticated*/ false);
                Err(err)
            }
            // Transient failures (e.g. a token refresh hiccup) aren't a signal
            // about login state, so they must not trip the auth breaker.
            Err(other) => Err(other),
        }
    }
}

impl ModelClientState {
    /// Drops the persisted provider conversation, so the next clamped turn
    /// starts a fresh Antigravity conversation instead of resuming a stale one.
    pub(super) fn clear_antigravity_conversation(&self) {
        if let Some(store) = self.antigravity_conversations.as_ref() {
            store.clear();
        }
    }
}
