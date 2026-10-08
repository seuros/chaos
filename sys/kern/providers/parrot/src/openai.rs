use std::sync::Arc;

use chaos_abi::AdapterFuture;
use chaos_abi::ModelAdapter;
use chaos_abi::TurnRequest;
use chaos_client::RequestTelemetry;

use crate::AuthProvider;
use crate::Provider;
use crate::RamaTransport;
use crate::ResponsesOptions;
use crate::SseTelemetry;
use crate::responses_adapter::ResponsesAdapter;

pub use crate::endpoint::responses::ResponsesWebSocket;

#[derive(Clone, Default)]
pub struct StaticAuthProvider {
    token: Option<String>,
    account_id: Option<String>,
}

impl std::fmt::Debug for StaticAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticAuthProvider")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("account_id", &self.account_id)
            .finish()
    }
}

impl StaticAuthProvider {
    pub fn new(token: Option<String>, account_id: Option<String>) -> Self {
        Self { token, account_id }
    }
}

impl AuthProvider for StaticAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        self.token.clone()
    }

    fn account_id(&self) -> Option<String> {
        self.account_id.clone()
    }
}

pub struct OpenAiAdapter<A: AuthProvider> {
    responses: ResponsesAdapter<A>,
}

impl<A: AuthProvider> std::fmt::Debug for OpenAiAdapter<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiAdapter")
            .field("default_model", &self.responses.default_model)
            .field("options", &"<responses-options>")
            .finish()
    }
}

impl OpenAiAdapter<StaticAuthProvider> {
    /// Convenience constructor for standalone / test use.
    /// Uses Kimi's dialect for its official endpoints, otherwise defaults to
    /// the OpenAI representer (`system` → `developer`).
    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_model: Option<String>,
    ) -> Self {
        let representer = if crate::representer::is_kimi_endpoint(&base_url) {
            crate::representer::SessionRepresenter::for_compatible_endpoint(&base_url)
        } else {
            crate::representer::SessionRepresenter::openai()
        };
        let provider =
            Provider::from_base_url_with_default_streaming_config("OpenAI", base_url, false);
        let auth = StaticAuthProvider::new(Some(api_key), None);
        Self::new(
            RamaTransport::default_client(),
            provider,
            auth,
            default_model,
            representer,
        )
    }
}

impl<A: AuthProvider> OpenAiAdapter<A> {
    pub fn new(
        transport: RamaTransport,
        provider: Provider,
        auth: A,
        default_model: Option<String>,
        representer: crate::representer::SessionRepresenter,
    ) -> Self {
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
        let options = self.responses.prepare_turn(request);
        // The subscription catalog can advertise text_and_image even though
        // its Responses endpoint rejects image search. Omitting this field
        // keeps the endpoint's text-only default, without pinning the catalog
        // or changing tools sent to the public API and compatible providers.
        if self.responses.discovery_base_url.trim_end_matches('/')
            == chaos_services::openai::CHATGPT_BACKEND_BASE
            && let Some(serde_json::Value::Array(tools)) =
                request.extensions.get_mut("openai_tools")
        {
            for tool in tools {
                if let Some(tool) = tool.as_object_mut()
                    && tool.get("type").and_then(serde_json::Value::as_str) == Some("web_search")
                {
                    tool.remove("search_content_types");
                }
            }
        }
        options
    }
}

impl<A> ModelAdapter for OpenAiAdapter<A>
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
        "OpenAI"
    }

    fn capabilities(&self) -> chaos_abi::AdapterCapabilities {
        chaos_abi::AdapterCapabilities {
            can_list_models: true,
        }
    }

    fn list_models(&self) -> chaos_abi::ListModelsFuture<'_> {
        Box::pin(async move { self.responses.list_models(Default::default()).await })
    }
}

#[cfg(test)]
mod tests;
