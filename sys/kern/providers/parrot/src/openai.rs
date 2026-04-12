use std::sync::Arc;

use chaos_abi::AbiError;
use chaos_abi::AdapterFuture;
use chaos_abi::ModelAdapter;
use chaos_abi::TurnEvent;
use chaos_abi::TurnRequest;
use chaos_abi::TurnStream;
use codex_client::RequestTelemetry;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::AuthProvider;
use crate::Provider;
use crate::RamaTransport;
use crate::ResponsesApiRequest;
use crate::ResponsesClient;
use crate::ResponsesOptions;
use crate::SseTelemetry;
use crate::requests::responses::Compression;

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
    client: ResponsesClient<RamaTransport, A>,
    options: ResponsesOptions,
    default_model: Option<String>,
    /// Base URL captured before the provider is consumed by `ResponsesClient`.
    /// Used exclusively for model discovery (`GET {base_url}/models`).
    discovery_base_url: String,
    /// Bearer token captured from the auth provider at construction time.
    discovery_token: Option<String>,
}

impl<A: AuthProvider> std::fmt::Debug for OpenAiAdapter<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiAdapter")
            .field("default_model", &self.default_model)
            .field("options", &"<responses-options>")
            .finish()
    }
}

impl OpenAiAdapter<StaticAuthProvider> {
    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_model: Option<String>,
    ) -> Self {
        let provider =
            Provider::from_base_url_with_default_streaming_config("OpenAI", base_url, false);
        let auth = StaticAuthProvider::new(Some(api_key), None);
        Self::new(
            RamaTransport::default_client(),
            provider,
            auth,
            default_model,
        )
    }
}

impl<A: AuthProvider> OpenAiAdapter<A> {
    pub fn new(
        transport: RamaTransport,
        provider: Provider,
        auth: A,
        default_model: Option<String>,
    ) -> Self {
        let discovery_base_url = provider.base_url.clone();
        let discovery_token = auth.bearer_token();
        Self {
            client: ResponsesClient::new(transport, provider, auth),
            options: ResponsesOptions::default(),
            default_model,
            discovery_base_url,
            discovery_token,
        }
    }

    pub fn with_options(mut self, options: ResponsesOptions) -> Self {
        self.options = options;
        self
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            client: self.client.with_telemetry(request, sse),
            options: self.options,
            default_model: self.default_model,
            discovery_base_url: self.discovery_base_url,
            discovery_token: self.discovery_token,
        }
    }
}

impl<A> ModelAdapter for OpenAiAdapter<A>
where
    A: AuthProvider + Send + Sync + 'static,
{
    fn stream(&self, mut request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            if request.model.is_empty()
                && let Some(default_model) = self.default_model.as_ref()
            {
                request.model = default_model.clone();
            }

            let options = responses_options_from_turn_request(&request, self.options.clone());
            let api_request: ResponsesApiRequest = request.into();
            let api_stream = self
                .client
                .stream_request(api_request, options)
                .await
                .map_err(AbiError::from)?;

            let (tx_event, rx_event) = mpsc::channel(1600);
            tokio::spawn(async move {
                let mut api_stream = api_stream;
                use futures::StreamExt;
                while let Some(event) = api_stream.next().await {
                    let mapped = event.map(TurnEvent::from).map_err(AbiError::from);
                    if tx_event.send(mapped).await.is_err() {
                        return;
                    }
                }
            });

            Ok(TurnStream { rx_event })
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
        let base_url = self.discovery_base_url.clone();
        let token = self.discovery_token.clone();
        Box::pin(async move { fetch_openai_models(&base_url, token.as_deref()).await })
    }
}

fn responses_options_from_turn_request(
    request: &TurnRequest,
    mut options: ResponsesOptions,
) -> ResponsesOptions {
    if request.extensions.contains_key("request_headers") {
        options.extra_headers = parse_request_headers(request.extensions.get("request_headers"));
    }
    if request.extensions.contains_key("compression") {
        options.compression = parse_compression(request.extensions.get("compression"));
    }
    if request.turn_state.is_some() {
        options.turn_state = request.turn_state.clone();
    }
    options
}

fn parse_request_headers(value: Option<&Value>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let Some(Value::Object(entries)) = value else {
        return headers;
    };
    for (name, value) in entries {
        if let Some(value) = value.as_str()
            && let (Ok(name), Ok(value)) = (
                HeaderName::try_from(name.as_str()),
                HeaderValue::from_str(value),
            )
        {
            headers.insert(name, value);
        }
    }
    headers
}

fn parse_compression(value: Option<&Value>) -> Compression {
    match value.and_then(Value::as_str) {
        Some("zstd") => Compression::Zstd,
        _ => Compression::None,
    }
}

// ── Model discovery ────────────────────────────────────────────────────────

/// Detect native server-side tools a provider supports based on its base URL.
///
/// xAI exposes `web_search` and `x_search` as Responses-API server-side tools.
/// Other OpenAI-compat providers typically do not, so we default to nothing.
fn native_tools_for_base_url(base_url: &str) -> Vec<String> {
    if base_url.contains("x.ai") {
        vec!["web_search".to_string(), "x_search".to_string()]
    } else {
        vec![]
    }
}

/// Fetch models from an OpenAI-compatible `GET /models` endpoint.
///
/// The wire format is `{ "object": "list", "data": [{ "id", "object",
/// "created", "owned_by" }] }`. OpenAI does not expose capability metadata
/// here, so all `supports_*` fields default to `false` and token limits are
/// left as `None`. Kern converts the result via `model_info_from_abi`, which
/// fills in safe defaults — crucially without setting `used_fallback_model_metadata`,
/// so the "Model metadata not found" warning is suppressed for known slugs.
///
/// This covers OpenAI, xAI/Grok, DeepSeek, and any other provider that
/// implements the OpenAI-compat `/models` endpoint.
async fn fetch_openai_models(
    base_url: &str,
    token: Option<&str>,
) -> Result<Vec<chaos_abi::AbiModelInfo>, chaos_abi::ListModelsError> {
    use rama::Service;
    use rama::http::Body;
    use rama::http::Request;
    use rama::http::StatusCode;
    use rama::http::body::util::BodyExt;
    use rama::http::client::EasyHttpWebClient;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct ModelsListResponse {
        data: Vec<ModelEntry>,
    }

    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }

    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let mut builder = Request::builder().method("GET").uri(&url);
    if let Some(token) = token {
        let bearer = format!("Bearer {token}");
        builder = builder.header(http::header::AUTHORIZATION, bearer);
    }
    let request = builder
        .body(Body::empty())
        .map_err(|e| chaos_abi::ListModelsError::Failed {
            message: e.to_string(),
        })?;

    let client = EasyHttpWebClient::default();
    let response = client
        .serve(request)
        .await
        .map_err(|e| chaos_abi::ListModelsError::Failed {
            message: format!("transport: {e}"),
        })?;

    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(chaos_abi::ListModelsError::Unsupported);
    }
    if !status.is_success() {
        let body = response
            .into_body()
            .collect()
            .await
            .map(|b| String::from_utf8_lossy(&b.to_bytes()).to_string())
            .unwrap_or_default();
        return Err(chaos_abi::ListModelsError::Failed {
            message: format!("HTTP {status}: {body}"),
        });
    }

    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| chaos_abi::ListModelsError::Failed {
            message: e.to_string(),
        })?
        .to_bytes();

    let resp: ModelsListResponse =
        serde_json::from_slice(&body).map_err(|e| chaos_abi::ListModelsError::Failed {
            message: format!("parse: {e}"),
        })?;

    let native_tools = native_tools_for_base_url(base_url);
    let models = resp
        .data
        .into_iter()
        .map(|m| chaos_abi::AbiModelInfo {
            display_name: m.id.clone(),
            id: m.id,
            max_input_tokens: None,
            max_output_tokens: None,
            supports_thinking: false,
            supports_images: false,
            supports_structured_output: false,
            supports_reasoning_effort: false,
            native_server_side_tools: native_tools.clone(),
        })
        .collect();

    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::protocol::SessionSource;
    use std::sync::OnceLock;

    #[test]
    fn responses_options_preserve_conversation_and_session_when_not_overridden() {
        let mut options = ResponsesOptions {
            conversation_id: Some("conv-123".to_string()),
            session_source: Some(SessionSource::Cli),
            ..ResponsesOptions::default()
        };
        options.extra_headers.insert(
            HeaderName::from_static("x-test"),
            HeaderValue::from_static("present"),
        );

        let request = TurnRequest {
            model: "gpt-5".to_string(),
            instructions: String::new(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        };

        let resolved = responses_options_from_turn_request(&request, options);

        assert_eq!(resolved.conversation_id.as_deref(), Some("conv-123"));
        assert_eq!(resolved.session_source, Some(SessionSource::Cli));
        assert_eq!(
            resolved
                .extra_headers
                .get("x-test")
                .and_then(|value| value.to_str().ok()),
            Some("present")
        );
    }

    #[test]
    fn responses_options_apply_request_level_overrides() {
        let turn_state = Arc::new(OnceLock::new());
        let mut extensions = serde_json::Map::new();
        extensions.insert(
            "request_headers".to_string(),
            serde_json::json!({"x-test": "override"}),
        );
        extensions.insert("compression".to_string(), serde_json::json!("zstd"));
        let request = TurnRequest {
            model: "gpt-5".to_string(),
            instructions: String::new(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: Some(turn_state.clone()),
            extensions,
        };

        let resolved = responses_options_from_turn_request(&request, ResponsesOptions::default());

        assert_eq!(
            resolved
                .extra_headers
                .get("x-test")
                .and_then(|value| value.to_str().ok()),
            Some("override")
        );
        assert!(matches!(resolved.compression, Compression::Zstd));
        assert!(
            resolved
                .turn_state
                .as_ref()
                .is_some_and(|state| Arc::ptr_eq(state, &turn_state))
        );
    }
}
