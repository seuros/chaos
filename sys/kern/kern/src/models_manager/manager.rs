use crate::api_bridge::auth_provider_from_auth;
use crate::api_bridge::map_api_error;
use crate::auth::AuthManager;
use crate::auth::AuthMode;
use crate::auth::ChaosAuth;
use crate::collaboration_modes::CollaborationModesConfig;
use crate::collaboration_modes::builtin_collaboration_mode_presets;
use crate::config::Config;
use crate::error::ChaosErr;
use crate::error::Result as CoreResult;
use crate::model_provider_info::ModelProviderInfo;
use crate::models_manager::model_info;
use crate::response_debug_context::extract_response_debug_context;
use crate::response_debug_context::telemetry_transport_error_message;
use crate::util::FeedbackRequestTags;
use crate::util::emit_feedback_request_tags;
use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::openai_models::ModelsResponse;
use chaos_model_catalog::ModelDiscoveryWorkflow;
use chaos_model_catalog::ModelsCache;
use chaos_model_catalog::ModelsCacheManager;
use chaos_model_catalog::ModelsCacheScope;
use chaos_parrot::ModelsClient;
use chaos_parrot::RamaTransport;
use chaos_parrot::RequestTelemetry;
use chaos_parrot::TransportError;
use chaos_syslog::TelemetryAuthMode;
use chrono_machines::ExponentialBackoff;
use chrono_machines::backoff::BackoffStrategy;
use http::HeaderMap;
use rand::make_rng;
use rand::rngs::StdRng;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::sync::TryLockError;
use tokio::time::timeout;
use tracing::error;
use tracing::info;
use tracing::instrument;
use tracing::warn;

const DEFAULT_MODEL_CACHE_TTL: Duration = Duration::from_secs(300);
const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const MODELS_ENDPOINT: &str = "/models";

enum FetchedCatalog {
    Live {
        models: Vec<ModelInfo>,
        etag: Option<String>,
    },
    Unsupported,
}

#[derive(Clone)]
struct ModelsRequestTelemetry {
    auth_mode: Option<String>,
    auth_header_attached: bool,
    auth_header_name: Option<&'static str>,
}

impl RequestTelemetry for ModelsRequestTelemetry {
    fn on_request(
        &self,
        attempt: u64,
        status: Option<http::StatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    ) {
        let success = status.is_some_and(|code| code.is_success()) && error.is_none();
        let error_message = error.map(telemetry_transport_error_message);
        let response_debug = error
            .map(extract_response_debug_context)
            .unwrap_or_default();
        let status = status.map(|status| status.as_u16());
        tracing::event!(
            target: "chaos_syslog.log_only",
            tracing::Level::INFO,
            event.name = "chaos.api_request",
            duration_ms = %duration.as_millis(),
            http.response.status_code = status,
            success = success,
            error.message = error_message.as_deref(),
            attempt = attempt,
            endpoint = MODELS_ENDPOINT,
            auth.header_attached = self.auth_header_attached,
            auth.header_name = self.auth_header_name,
            auth.request_id = response_debug.request_id.as_deref(),
            auth.cf_ray = response_debug.cf_ray.as_deref(),
            auth.error = response_debug.auth_error.as_deref(),
            auth.error_code = response_debug.auth_error_code.as_deref(),
            auth.mode = self.auth_mode.as_deref(),
        );
        tracing::event!(
            target: "chaos_syslog.trace_safe",
            tracing::Level::INFO,
            event.name = "chaos.api_request",
            duration_ms = %duration.as_millis(),
            http.response.status_code = status,
            success = success,
            error.message = error_message.as_deref(),
            attempt = attempt,
            endpoint = MODELS_ENDPOINT,
            auth.header_attached = self.auth_header_attached,
            auth.header_name = self.auth_header_name,
            auth.request_id = response_debug.request_id.as_deref(),
            auth.cf_ray = response_debug.cf_ray.as_deref(),
            auth.error = response_debug.auth_error.as_deref(),
            auth.error_code = response_debug.auth_error_code.as_deref(),
            auth.mode = self.auth_mode.as_deref(),
        );
        emit_feedback_request_tags(&FeedbackRequestTags {
            endpoint: MODELS_ENDPOINT,
            auth_header_attached: self.auth_header_attached,
            auth_header_name: self.auth_header_name,
            auth_mode: self.auth_mode.as_deref(),
            auth_retry_after_unauthorized: None,
            auth_recovery_mode: None,
            auth_recovery_phase: None,
            auth_connection_reused: None,
            auth_request_id: response_debug.request_id.as_deref(),
            auth_cf_ray: response_debug.cf_ray.as_deref(),
            auth_error: response_debug.auth_error.as_deref(),
            auth_error_code: response_debug.auth_error_code.as_deref(),
            auth_recovery_followup_success: None,
            auth_recovery_followup_status: None,
        });
    }
}

pub use chaos_model_catalog::RefreshStrategy;

/// How the manager's base catalog is sourced for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogMode {
    /// Start from an empty catalog and populate via cache/network refresh.
    Default,
    /// Use a caller-provided catalog as authoritative and do not mutate it via refresh.
    Custom,
}

/// Coordinates remote model discovery plus cached metadata on disk.
#[derive(Debug)]
pub struct ModelsManager {
    remote_models: RwLock<Vec<ModelInfo>>,
    catalog_mode: CatalogMode,
    collaboration_modes_config: CollaborationModesConfig,
    auth_manager: Arc<AuthManager>,
    etag: RwLock<Option<String>>,
    cache_manager: ModelsCacheManager,
    provider: ModelProviderInfo,
}

impl ModelsManager {
    /// Construct a manager scoped to the provided `AuthManager`.
    ///
    /// Uses `chaos_home` to store cached model metadata and initializes with bundled catalog
    /// When `model_catalog` is provided, it becomes the authoritative remote model list and
    /// background refreshes from `/models` are disabled.
    pub fn new(
        chaos_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        model_catalog: Option<ModelsResponse>,
        collaboration_modes_config: CollaborationModesConfig,
    ) -> Self {
        Self::new_with_provider(
            chaos_home,
            auth_manager,
            model_catalog,
            collaboration_modes_config,
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        )
    }

    /// Construct a manager with an explicit provider used for remote model refreshes.
    pub fn new_with_provider(
        chaos_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        model_catalog: Option<ModelsResponse>,
        collaboration_modes_config: CollaborationModesConfig,
        provider: ModelProviderInfo,
    ) -> Self {
        let cache_manager = ModelsCacheManager::new(chaos_home, DEFAULT_MODEL_CACHE_TTL);
        let catalog_mode = if model_catalog.is_some() {
            CatalogMode::Custom
        } else {
            CatalogMode::Default
        };
        // Start with the provided catalog, or an empty list. The adapter
        // fetch populates this on first successful refresh. No bundled
        // fallback — the OS is provider-agnostic and does not ship a
        // hardcoded model catalog.
        let remote_models = model_catalog
            .map(|catalog| catalog.models)
            .unwrap_or_default();
        Self {
            remote_models: RwLock::new(remote_models),
            catalog_mode,
            collaboration_modes_config,
            auth_manager,
            etag: RwLock::new(None),
            cache_manager,
            provider,
        }
    }

    /// List all available models, refreshing according to the specified strategy.
    ///
    /// Returns model presets sorted by priority and filtered by auth mode and visibility.
    #[instrument(
        level = "info",
        skip(self),
        fields(refresh_strategy = %refresh_strategy)
    )]
    pub async fn list_models(&self, refresh_strategy: RefreshStrategy) -> Vec<ModelPreset> {
        if let Err(err) = self.refresh_available_models(refresh_strategy).await {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models().await;
        self.build_available_models(remote_models)
    }

    /// List collaboration mode presets.
    ///
    /// Returns a static set of presets seeded with the configured model.
    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.list_collaboration_modes_for_config(self.collaboration_modes_config)
    }

    pub fn list_collaboration_modes_for_config(
        &self,
        collaboration_modes_config: CollaborationModesConfig,
    ) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets(collaboration_modes_config)
    }

    /// Attempt to list models without blocking, using the current cached state.
    ///
    /// Returns an error if the internal lock cannot be acquired.
    pub fn try_list_models(&self) -> Result<Vec<ModelPreset>, TryLockError> {
        let remote_models = self.try_get_remote_models()?;
        Ok(self.build_available_models(remote_models))
    }

    // todo(aibrahim): should be visible to core only and sent on session_configured event
    /// Get the model identifier to use, refreshing according to the specified strategy.
    ///
    /// If `model` is provided, returns it directly. Otherwise selects the default based on
    /// auth mode and available models.
    #[instrument(
        level = "info",
        skip(self, model),
        fields(
            model.provided = model.is_some(),
            refresh_strategy = %refresh_strategy
        )
    )]
    pub async fn get_default_model(
        &self,
        model: &Option<String>,
        refresh_strategy: RefreshStrategy,
    ) -> String {
        if let Some(model) = model.as_ref() {
            return model.to_string();
        }
        if let Err(err) = self.refresh_available_models(refresh_strategy).await {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models().await;
        let available = self.build_available_models(remote_models);
        available
            .iter()
            .find(|model| model.is_default)
            .or_else(|| available.first())
            .map(|model| model.model.clone())
            .unwrap_or_default()
    }

    // todo(aibrahim): look if we can tighten it to pub(crate)
    /// Look up model metadata, applying remote overrides and config adjustments.
    ///
    /// For Anthropic providers, triggers an on-demand refresh so that models
    /// fetched from the adapter are available for lookup.
    #[instrument(level = "info", skip(self, config), fields(model = model))]
    pub async fn get_model_info(&self, model: &str, config: &Config) -> ModelInfo {
        if crate::model_provider_info::is_anthropic_wire(self.provider.base_url.as_deref())
            && let Err(err) = self
                .refresh_available_models(RefreshStrategy::OnlineIfUncached)
                .await
        {
            error!("failed to refresh Anthropic models: {err}");
        }
        let remote_models = self.get_remote_models().await;
        Self::construct_model_info_from_candidates(model, &remote_models, config)
    }

    fn find_model_by_longest_prefix(model: &str, candidates: &[ModelInfo]) -> Option<ModelInfo> {
        let mut best: Option<ModelInfo> = None;
        for candidate in candidates {
            if !model.starts_with(&candidate.slug) {
                continue;
            }
            let is_better_match = if let Some(current) = best.as_ref() {
                candidate.slug.len() > current.slug.len()
            } else {
                true
            };
            if is_better_match {
                best = Some(candidate.clone());
            }
        }
        best
    }

    /// Retry metadata lookup for a single namespaced slug like `namespace/model-name`.
    ///
    /// This only strips one leading namespace segment and only when the namespace is ASCII
    /// alphanumeric/underscore (`\\w+`) to avoid broadly matching arbitrary aliases.
    fn find_model_by_namespaced_suffix(model: &str, candidates: &[ModelInfo]) -> Option<ModelInfo> {
        let (namespace, suffix) = model.split_once('/')?;
        if suffix.contains('/') {
            return None;
        }
        if !namespace
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return None;
        }
        Self::find_model_by_longest_prefix(suffix, candidates)
    }

    fn construct_model_info_from_candidates(
        model: &str,
        candidates: &[ModelInfo],
        config: &Config,
    ) -> ModelInfo {
        // First use the normal longest-prefix match. If that misses, allow a narrowly scoped
        // retry for namespaced slugs like `custom/gpt-5.3-codex`.
        let remote = Self::find_model_by_longest_prefix(model, candidates)
            .or_else(|| Self::find_model_by_namespaced_suffix(model, candidates));
        let model_info = if let Some(remote) = remote {
            ModelInfo {
                slug: model.to_string(),
                used_fallback_model_metadata: false,
                ..remote
            }
        } else {
            // No catalog entry for this slug — build a minimal descriptor from
            // the slug itself. Mark as fallback so callers can detect the gap.
            // Derive native server-side tools from the provider URL so that
            // unknown/new models on known providers (e.g. a fresh xAI model not
            // yet in the catalog) still get the correct tool set and don't have
            // client-managed web_search injected with `external_web_access`,
            // which xAI rejects.
            let native_server_side_tools =
                crate::model_provider_info::native_server_side_tools_for_url(
                    config.model_provider.base_url.as_deref(),
                );
            ModelInfo {
                used_fallback_model_metadata: true,
                ..model_info::model_info_from_abi(&chaos_abi::AbiModelInfo {
                    id: model.to_string(),
                    display_name: model.to_string(),
                    max_input_tokens: None,
                    max_output_tokens: None,
                    supports_thinking: false,
                    supports_images: true,
                    supports_structured_output: false,
                    supports_reasoning_effort: false,
                    native_server_side_tools,
                })
            }
        };
        model_info::with_config_overrides(model_info, config)
    }

    /// Refresh models if the provided ETag differs from the cached ETag.
    ///
    /// Uses `Online` strategy to fetch latest models when ETags differ.
    pub(crate) async fn refresh_if_new_etag(&self, etag: String) {
        let current_etag = self.get_etag().await;
        if current_etag.clone().is_some() && current_etag.as_deref() == Some(etag.as_str()) {
            if let Err(err) = self
                .cache_manager
                .renew_cache_ttl(&self.cache_scope())
                .await
            {
                error!("failed to renew cache TTL: {err}");
            }
            return;
        }
        if let Err(err) = self.refresh_available_models(RefreshStrategy::Online).await {
            error!("failed to refresh available models: {err}");
        }
    }

    /// Refresh available models according to the specified strategy.
    ///
    /// No bundled JSON fallback. If the DB is empty, we fetch from the
    /// provider. The adapter is the source of truth, the DB is the cache.
    async fn refresh_available_models(&self, refresh_strategy: RefreshStrategy) -> CoreResult<()> {
        // don't override the custom model catalog if one was provided by the user
        if matches!(self.catalog_mode, CatalogMode::Custom) {
            return Ok(());
        }

        let mut workflow = ModelDiscoveryWorkflow::new();
        workflow.begin(refresh_strategy);

        match refresh_strategy {
            RefreshStrategy::Offline => {
                if let Some(cache) = self.load_fresh_cache().await {
                    workflow.record_cache_hit();
                    self.apply_cache_entry(cache).await;
                } else {
                    workflow.record_cache_miss();
                }
                Ok(())
            }
            RefreshStrategy::OnlineIfUncached => {
                if let Some(cache) = self.load_fresh_cache().await {
                    workflow.record_cache_hit();
                    self.apply_cache_entry(cache).await;
                    info!("models cache: using cached models");
                    return Ok(());
                }
                workflow.record_cache_miss();
                info!("models cache: cache miss, fetching from provider");
                workflow.record_fetch_started();
                self.fetch_and_update_models(&mut workflow).await
            }
            RefreshStrategy::Online => self.fetch_and_update_models(&mut workflow).await,
        }
    }

    async fn fetch_and_update_models(
        &self,
        workflow: &mut ModelDiscoveryWorkflow,
    ) -> CoreResult<()> {
        let _timer =
            chaos_syslog::start_global_timer("chaos.remote_models.fetch_update.duration_ms", &[]);

        let backoff = ExponentialBackoff::new()
            .max_attempts(3)
            .base_delay_ms(100)
            .multiplier(2.0)
            .max_delay_ms(5000);
        let mut rng: StdRng = make_rng();
        let mut attempt: u8 = 0;
        let result = loop {
            attempt += 1;
            match self.fetch_catalog().await {
                ok @ Ok(_) => break ok,
                Err(err) if backoff.should_retry(attempt) => {
                    let delay_ms = backoff.delay(attempt, &mut rng).unwrap_or(100);
                    warn!(
                        attempt,
                        delay_ms, "model catalog fetch failed, retrying: {err}"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                err => break err,
            }
        };

        match result {
            Ok(FetchedCatalog::Live { models, etag }) => {
                workflow.record_live_catalog();
                self.apply_live_catalog(models, etag).await;
                Ok(())
            }
            Ok(FetchedCatalog::Unsupported) => {
                workflow.record_unsupported_catalog();
                self.apply_unsupported_catalog().await;
                Ok(())
            }
            Err(err) => {
                workflow.record_failed();
                Err(err)
            }
        }
    }

    async fn fetch_catalog(&self) -> CoreResult<FetchedCatalog> {
        // Anthropic-compatible providers get models through the AnthropicAdapter.
        if crate::model_provider_info::is_anthropic_wire(self.provider.base_url.as_deref()) {
            return self.fetch_catalog_via_adapter().await;
        }
        // The chaos backend always speaks the chaos-flavored ModelsResponse with
        // rich metadata — whether or not base_url is overridden (e.g. in tests).
        // Use ModelsClient for any openai-provider configuration.
        if self.provider.is_openai() {
            return self.fetch_catalog_via_models_client().await;
        }
        // All other OpenAI-compatible providers (xAI, DeepSeek, custom deployments, etc.)
        // speak the standard GET /models wire format. Use the adapter, cache in the DB.
        self.fetch_catalog_via_openai_adapter().await
    }

    async fn fetch_catalog_via_models_client(&self) -> CoreResult<FetchedCatalog> {
        let auth = self.auth_manager.auth().await;
        let auth_mode = auth.as_ref().map(ChaosAuth::auth_mode);
        let api_provider = self.provider.to_api_provider(auth_mode)?;
        let api_auth = auth_provider_from_auth(auth.clone(), &self.provider)?;
        let transport = RamaTransport::default_client();
        let request_telemetry: Arc<dyn RequestTelemetry> = Arc::new(ModelsRequestTelemetry {
            auth_mode: auth_mode.map(|mode| TelemetryAuthMode::from(mode).to_string()),
            auth_header_attached: api_auth.auth_header_attached(),
            auth_header_name: api_auth.auth_header_name(),
        });
        let client = ModelsClient::new(transport, api_provider, api_auth)
            .with_telemetry(Some(request_telemetry));

        let client_version = crate::models_manager::client_version_to_whole();
        let (models, etag) = timeout(
            MODELS_REFRESH_TIMEOUT,
            client.list_models(&client_version, HeaderMap::new()),
        )
        .await
        .map_err(|_| ChaosErr::Timeout)?
        .map_err(map_api_error)?;

        Ok(FetchedCatalog::Live { models, etag })
    }

    /// Fetch models via the ABI adapter (Anthropic-compatible providers).
    async fn fetch_catalog_via_adapter(&self) -> CoreResult<FetchedCatalog> {
        use chaos_abi::ListModelsError;
        use chaos_abi::ModelAdapter;
        use chaos_parrot::anthropic::AnthropicAdapter;
        use chaos_parrot::anthropic::AnthropicAuth;

        let auth_mode = self.auth_manager.auth().await;
        let auth_mode_ref = auth_mode.as_ref().map(ChaosAuth::auth_mode);
        let api_provider = self.provider.to_api_provider(auth_mode_ref)?;

        // Resolve auth the same way client.rs does.
        let adapter_auth = match self.provider.api_key() {
            Ok(Some(api_key)) => AnthropicAuth::ApiKey(api_key),
            Ok(None) => {
                if let Some(token) = self.provider.experimental_bearer_token.clone() {
                    AnthropicAuth::BearerToken(token)
                } else {
                    return Err(crate::api_bridge::provider_auth_missing(&self.provider));
                }
            }
            Err(ChaosErr::EnvVar(_)) => {
                return Err(crate::api_bridge::provider_auth_missing(&self.provider));
            }
            Err(other) => return Err(other),
        };

        let sniffer =
            chaos_libration::registry::sniffer_for("anthropic_messages", &api_provider.base_url);
        let adapter = AnthropicAdapter::new(api_provider, adapter_auth, None).with_sniffer(sniffer);

        let abi_models = timeout(MODELS_REFRESH_TIMEOUT, adapter.list_models())
            .await
            .map_err(|_| ChaosErr::Timeout)?;

        match abi_models {
            Ok(models) => {
                let kern_models: Vec<ModelInfo> =
                    models.iter().map(model_info::model_info_from_abi).collect();
                info!(
                    count = kern_models.len(),
                    "fetched models via Anthropic adapter"
                );
                Ok(FetchedCatalog::Live {
                    models: kern_models,
                    etag: None,
                })
            }
            Err(ListModelsError::Unsupported) => Ok(FetchedCatalog::Unsupported),
            Err(ListModelsError::Failed { message }) => {
                error!("Anthropic model listing failed: {message}");
                Err(ChaosErr::Stream(message, None))
            }
        }
    }

    /// Fetch models via the OpenAI-compat adapter for third-party providers.
    ///
    /// Hits `GET {base_url}/models`, parses the standard OpenAI list format
    /// (`{ data: [{ id, ... }] }`), converts each entry to `AbiModelInfo`,
    /// then to `ModelInfo` via `model_info_from_abi`. The result is written
    /// to the in-memory catalog and persisted to the SQLite cache.
    async fn fetch_catalog_via_openai_adapter(&self) -> CoreResult<FetchedCatalog> {
        use chaos_abi::ListModelsError;
        use chaos_abi::ModelAdapter;
        use chaos_parrot::openai::OpenAiAdapter;
        use chaos_parrot::openai::StaticAuthProvider;

        let auth = self.auth_manager.auth().await;
        let auth_mode = auth.as_ref().map(ChaosAuth::auth_mode);
        let api_provider = self.provider.to_api_provider(auth_mode)?;

        let token = match self.provider.api_key() {
            Ok(Some(api_key)) => Some(api_key),
            Ok(None) => self.provider.experimental_bearer_token.clone(),
            Err(ChaosErr::EnvVar(_)) => {
                return Err(crate::api_bridge::provider_auth_missing(&self.provider));
            }
            Err(other) => return Err(other),
        };

        let auth_provider = StaticAuthProvider::new(token, None);
        let representer = if self.provider.is_openai() {
            chaos_parrot::SessionRepresenter::openai()
        } else {
            chaos_parrot::SessionRepresenter::wannabe()
        };
        let adapter = OpenAiAdapter::new(
            chaos_parrot::RamaTransport::default_client(),
            api_provider,
            auth_provider,
            None,
            representer,
        );

        let abi_models = timeout(MODELS_REFRESH_TIMEOUT, adapter.list_models())
            .await
            .map_err(|_| ChaosErr::Timeout)?;

        match abi_models {
            Ok(models) => {
                let kern_models: Vec<ModelInfo> =
                    models.iter().map(model_info::model_info_from_abi).collect();
                info!(
                    count = kern_models.len(),
                    provider = self.provider.name,
                    "fetched models via OpenAI-compat adapter"
                );
                Ok(FetchedCatalog::Live {
                    models: kern_models,
                    etag: None,
                })
            }
            Err(ListModelsError::Unsupported) => Ok(FetchedCatalog::Unsupported),
            Err(ListModelsError::Failed { message }) => {
                error!(
                    provider = self.provider.name,
                    "OpenAI-compat model listing failed: {message}"
                );
                Err(ChaosErr::Stream(message, None))
            }
        }
    }

    async fn get_etag(&self) -> Option<String> {
        self.etag.read().await.clone()
    }

    async fn apply_live_catalog(&self, models: Vec<ModelInfo>, etag: Option<String>) {
        let client_version = crate::models_manager::client_version_to_whole();
        self.apply_remote_models(models.clone()).await;
        *self.etag.write().await = etag.clone();
        self.cache_manager
            .persist_cache(&models, etag, client_version, self.cache_scope())
            .await;
    }

    async fn apply_unsupported_catalog(&self) {
        let empty_models: Vec<ModelInfo> = Vec::new();
        let client_version = crate::models_manager::client_version_to_whole();
        info!("provider does not support model listing, caching empty provider catalog");
        self.apply_remote_models(empty_models.clone()).await;
        *self.etag.write().await = None;
        self.cache_manager
            .persist_cache(&empty_models, None, client_version, self.cache_scope())
            .await;
    }

    /// Replace the active catalog with the models fetched from the provider.
    ///
    /// No bundled fallback is merged — the provider's response is authoritative.
    async fn apply_remote_models(&self, models: Vec<ModelInfo>) {
        *self.remote_models.write().await = models;
    }

    async fn apply_cache_entry(&self, cache: ModelsCache) {
        let ModelsCache { models, etag, .. } = cache;
        *self.etag.write().await = etag.clone();
        self.apply_remote_models(models.clone()).await;
        info!(
            models_count = models.len(),
            etag = ?etag,
            "models cache: cache entry applied"
        );
    }

    /// Attempt to satisfy the refresh from the cache when it matches the provider and TTL.
    async fn load_fresh_cache(&self) -> Option<ModelsCache> {
        let _timer =
            chaos_syslog::start_global_timer("chaos.remote_models.load_cache.duration_ms", &[]);
        let client_version = crate::models_manager::client_version_to_whole();
        info!(client_version, "models cache: evaluating cache eligibility");
        let cache = match self
            .cache_manager
            .load_fresh(&client_version, &self.cache_scope())
            .await
        {
            Some(cache) => cache,
            None => {
                info!("models cache: no usable cache entry");
                return None;
            }
        };
        Some(cache)
    }

    fn cache_scope(&self) -> ModelsCacheScope {
        ModelsCacheScope {
            provider_name: self.provider.name.clone(),
            wire_api: self.provider.wire_api.to_string(),
            base_url: self
                .provider
                .effective_base_url(self.auth_manager.auth_mode()),
        }
    }

    /// Build picker-ready presets from the active catalog snapshot.
    fn build_available_models(&self, mut remote_models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        remote_models.sort_by_key(|a| a.priority);

        let mut presets: Vec<ModelPreset> = remote_models.into_iter().map(Into::into).collect();
        let chatgpt_mode = matches!(self.auth_manager.auth_mode(), Some(AuthMode::Chatgpt));
        presets = ModelPreset::filter_by_auth(presets, chatgpt_mode);

        ModelPreset::mark_default_by_picker_visibility(&mut presets);

        presets
    }

    async fn get_remote_models(&self) -> Vec<ModelInfo> {
        self.remote_models.read().await.clone()
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        Ok(self.remote_models.try_read()?.clone())
    }

    /// Construct a manager with a specific provider for testing.
    pub(crate) fn with_provider_for_tests(
        chaos_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        provider: ModelProviderInfo,
    ) -> Self {
        Self::new_with_provider(
            chaos_home,
            auth_manager,
            /*model_catalog*/ None,
            CollaborationModesConfig::default(),
            provider,
        )
    }

    /// Get model identifier without consulting remote state or cache.
    pub(crate) fn get_model_offline_for_tests(model: Option<&str>) -> String {
        model
            .map(String::from)
            .unwrap_or_else(|| "gpt-5.2-codex".to_string())
    }

    /// Build `ModelInfo` without consulting remote state or cache.
    pub(crate) fn construct_model_info_offline_for_tests(
        model: &str,
        config: &Config,
    ) -> ModelInfo {
        let candidates: &[ModelInfo] = if let Some(model_catalog) = config.model_catalog.as_ref() {
            &model_catalog.models
        } else {
            &[]
        };
        Self::construct_model_info_from_candidates(model, candidates, config)
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
