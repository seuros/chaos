//! xAI/Grok Responses dialect and subscription-proxy policy.

use std::sync::Arc;

use chaos_abi::AdapterFuture;
use chaos_abi::ModelAdapter;
use chaos_abi::TurnRequest;
use chaos_client::RequestTelemetry;
use rama::http::HeaderMap;
use rama::http::HeaderName;
use rama::http::HeaderValue;

use crate::AuthProvider;
use crate::Provider;
use crate::RamaTransport;
use crate::ResponsesOptions;
use crate::SseTelemetry;
use crate::openai::StaticAuthProvider;
use crate::responses_adapter::ResponsesAdapter;

pub use crate::endpoint::responses::ResponsesWebSocket;

const XAI_API_HOST: &str = "api.x.ai";
const GROK_SUBSCRIPTION_PROXY_HOST: &str = "cli-chat-proxy.grok.com";
const GROK_CLIENT_VERSION_HEADER: &str = "x-grok-client-version";
const GROK_MODEL_OVERRIDE_HEADER: &str = "x-grok-model-override";
const XAI_TOKEN_AUTH_HEADER: &str = "x-xai-token-auth";
const XAI_TOKEN_AUTH_VALUE: &str = "xai-grok-cli";

/// Recognize the official xAI API and Grok subscription proxy by exact host.
///
/// Provider identity can still select this adapter for a custom endpoint;
/// recognition must never grant subscription policy to a lookalike URL.
pub fn is_xai_endpoint(base_url: &str) -> bool {
    url::Url::parse(base_url).is_ok_and(|url| {
        matches!(
            url.host_str(),
            Some(XAI_API_HOST | GROK_SUBSCRIPTION_PROXY_HOST)
        )
    })
}

fn is_grok_subscription_proxy(base_url: &str) -> bool {
    url::Url::parse(base_url).is_ok_and(|url| url.host_str() == Some(GROK_SUBSCRIPTION_PROXY_HOST))
}

pub struct XaiAdapter<A: AuthProvider> {
    responses: ResponsesAdapter<A>,
}

impl<A: AuthProvider> std::fmt::Debug for XaiAdapter<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XaiAdapter")
            .field("default_model", &self.responses.default_model)
            .field("options", &"<responses-options>")
            .finish()
    }
}

impl XaiAdapter<StaticAuthProvider> {
    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_model: Option<String>,
    ) -> Self {
        let provider =
            Provider::from_base_url_with_default_streaming_config("xAI", base_url, false);
        Self::new(
            RamaTransport::default_client(),
            provider,
            StaticAuthProvider::new(Some(api_key), None),
            default_model,
            crate::representer::SessionRepresenter::wannabe(),
        )
    }
}

impl<A: AuthProvider> XaiAdapter<A> {
    pub fn new(
        transport: RamaTransport,
        mut provider: Provider,
        auth: A,
        default_model: Option<String>,
        representer: crate::representer::SessionRepresenter,
    ) -> Self {
        // Token-auth is a provider default, unlike the routing/version headers:
        // per-turn request_headers must retain their existing precedence.
        if is_grok_subscription_proxy(&provider.base_url) {
            provider.headers.insert(
                HeaderName::from_static(XAI_TOKEN_AUTH_HEADER),
                HeaderValue::from_static(XAI_TOKEN_AUTH_VALUE),
            );
        }
        Self {
            responses: ResponsesAdapter::new(transport, provider, auth, default_model, representer),
        }
    }

    pub fn with_options(mut self, options: ResponsesOptions) -> Self {
        self.responses = self.responses.with_options(options);
        self
    }

    pub fn with_websocket(mut self, websocket: Arc<ResponsesWebSocket>) -> Self {
        self.responses = self.responses.with_websocket(websocket);
        self
    }

    /// Socket-only prewarm: no generation, request body, or auth refresh.
    pub fn prewarm(&self) {
        self.responses.prewarm();
    }

    pub fn with_telemetry(
        mut self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        self.responses = self.responses.with_telemetry(request, sse);
        self
    }

    fn prepare_turn(&self, request: &mut TurnRequest) -> ResponsesOptions {
        let mut options = self.responses.prepare_turn(request);
        insert_grok_subscription_headers(
            &self.responses.discovery_base_url,
            &request.model,
            &mut options.extra_headers,
        );
        options
    }
}

impl<A> ModelAdapter for XaiAdapter<A>
where
    A: AuthProvider + Send + Sync + 'static,
{
    fn stream(&self, mut request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            let options = self.prepare_turn(&mut request);
            self.responses.stream(request, options).await
        })
    }

    fn provider_name(&self) -> &str {
        "xAI"
    }

    fn capabilities(&self) -> chaos_abi::AdapterCapabilities {
        chaos_abi::AdapterCapabilities {
            can_list_models: true,
        }
    }

    fn list_models(&self) -> chaos_abi::ListModelsFuture<'_> {
        Box::pin(async move {
            let base_url = &self.responses.discovery_base_url;
            let mut headers = HeaderMap::new();
            insert_grok_subscription_auth_headers(base_url, &mut headers);
            let mut models = self.responses.list_models(headers).await?;
            apply_model_policy(base_url, &mut models);
            Ok(models)
        })
    }
}

fn insert_grok_subscription_auth_headers(base_url: &str, headers: &mut HeaderMap) {
    if !is_grok_subscription_proxy(base_url) {
        return;
    }
    headers.insert(
        HeaderName::from_static(XAI_TOKEN_AUTH_HEADER),
        HeaderValue::from_static(XAI_TOKEN_AUTH_VALUE),
    );
    headers.insert(
        HeaderName::from_static(GROK_CLIENT_VERSION_HEADER),
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
}

fn insert_grok_subscription_headers(base_url: &str, model: &str, headers: &mut HeaderMap) {
    if !is_grok_subscription_proxy(base_url) {
        return;
    }
    headers.insert(
        HeaderName::from_static(GROK_CLIENT_VERSION_HEADER),
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    if let Ok(model) = HeaderValue::from_str(model) {
        headers.insert(HeaderName::from_static(GROK_MODEL_OVERRIDE_HEADER), model);
    }
}

/// The proxy's listing omits image support for Grok 4 chat models even though
/// those models accept images. Coding models are excluded from the fallback.
fn grok_proxy_model_accepts_images(base_url: &str, id: &str) -> bool {
    if !is_grok_subscription_proxy(base_url) {
        return false;
    }
    let id = id.to_ascii_lowercase();
    let grok_4 = id == "grok-4" || id.starts_with("grok-4.") || id.starts_with("grok-4-");
    grok_4 && !id.contains("code")
}

fn resolve_supports_images(base_url: &str, id: &str, declared: Option<bool>) -> bool {
    declared == Some(true) || grok_proxy_model_accepts_images(base_url, id)
}

/// Native Responses tools are known on the API, not on the subscription proxy
/// or arbitrary custom endpoints (including lookalike hosts).
pub fn native_tools_for_base_url(base_url: &str) -> Vec<String> {
    if url::Url::parse(base_url).is_ok_and(|url| url.host_str() == Some(XAI_API_HOST)) {
        vec!["web_search".to_string(), "x_search".to_string()]
    } else {
        vec![]
    }
}

fn apply_model_policy(base_url: &str, models: &mut [chaos_abi::AbiModelInfo]) {
    let native_tools = native_tools_for_base_url(base_url);
    for model in models {
        model.supports_images =
            resolve_supports_images(base_url, &model.id, Some(model.supports_images));
        model.native_server_side_tools = native_tools.clone();
    }
}

#[cfg(test)]
mod tests;
