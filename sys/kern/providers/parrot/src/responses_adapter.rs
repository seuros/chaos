//! Provider-neutral Responses streaming and OpenAI-compatible model discovery.
//!
//! Adapters own dialect policy; this module only applies turn options, projects
//! ABI items, delivers events, and carries declared discovery metadata.

use std::sync::Arc;

use chaos_abi::AbiError;
use chaos_abi::TurnEvent;
use chaos_abi::TurnRequest;
use chaos_abi::TurnStream;
use chaos_client::RequestTelemetry;
use rama::http::HeaderMap;
use rama::http::HeaderName;
use rama::http::HeaderValue;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::AuthProvider;
use crate::Provider;
use crate::RamaTransport;
use crate::ResponsesClient;
use crate::ResponsesOptions;
use crate::SseTelemetry;
use crate::endpoint::responses::ResponsesWebSocket;
use crate::requests::responses::Compression;

pub(crate) struct ResponsesAdapter<A: AuthProvider> {
    client: ResponsesClient<RamaTransport, A>,
    options: ResponsesOptions,
    pub(crate) default_model: Option<String>,
    /// Captured before the provider is consumed by `ResponsesClient`.
    pub(crate) discovery_base_url: String,
    /// Captured at construction, as in the original Responses adapter.
    discovery_token: Option<String>,
    discovery_egress: Option<chaos_client::Egress>,
    representer: crate::representer::SessionRepresenter,
}

impl<A: AuthProvider> ResponsesAdapter<A> {
    pub(crate) fn new(
        transport: RamaTransport,
        provider: Provider,
        auth: A,
        default_model: Option<String>,
        representer: crate::representer::SessionRepresenter,
    ) -> Self {
        let discovery_base_url = provider.base_url.clone();
        let discovery_token = auth.bearer_token();
        let discovery_egress = provider.egress.clone();
        Self {
            client: ResponsesClient::new(transport, provider, auth),
            options: ResponsesOptions::default(),
            default_model,
            discovery_base_url,
            discovery_token,
            discovery_egress,
            representer,
        }
    }

    pub(crate) fn with_options(mut self, options: ResponsesOptions) -> Self {
        self.options = options;
        self
    }

    pub(crate) fn with_websocket(mut self, websocket: Arc<ResponsesWebSocket>) -> Self {
        self.client = self.client.with_websocket(websocket);
        self
    }

    pub(crate) fn prewarm(&self) {
        self.client.prewarm(&self.options);
    }

    pub(crate) fn with_telemetry(
        mut self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        self.client = self.client.with_telemetry(request, sse);
        self
    }

    /// Resolve default model and per-turn overrides before dialect policy.
    pub(crate) fn prepare_turn(&self, request: &mut TurnRequest) -> ResponsesOptions {
        if request.model.is_empty()
            && let Some(default_model) = self.default_model.as_ref()
        {
            request.model = default_model.clone();
        }
        responses_options_from_turn_request(request, self.options.clone())
    }

    pub(crate) async fn stream(
        &self,
        request: TurnRequest,
        options: ResponsesOptions,
    ) -> Result<TurnStream, AbiError> {
        let api_request =
            crate::adapter::turn_request_to_api_request(request, self.representer.as_representer());
        let api_stream = self
            .client
            .stream_request(api_request, options)
            .await
            .map_err(AbiError::from)?;

        let (tx_event, rx_event) = mpsc::channel(1600);
        tokio::spawn(async move {
            let mut api_stream = api_stream;
            use futures::StreamExt;
            loop {
                let event = tokio::select! {
                    _ = tx_event.closed() => return,
                    event = api_stream.next() => event,
                };
                let Some(event) = event else { return };
                let mapped = event.map(TurnEvent::from).map_err(AbiError::from);
                if tx_event.send(mapped).await.is_err() {
                    return;
                }
            }
        });

        Ok(TurnStream { rx_event })
    }

    pub(crate) async fn list_models(
        &self,
        headers: HeaderMap,
    ) -> Result<Vec<chaos_abi::AbiModelInfo>, chaos_abi::ListModelsError> {
        fetch_models(
            &self.discovery_base_url,
            self.discovery_token.as_deref(),
            self.discovery_egress.clone(),
            headers,
        )
        .await
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

/// Families that share a `/models` listing with chat models while being unable
/// to answer a prompt. Matched as substrings of the model id.
const NON_CONVERSATIONAL_MARKERS: &[&str] = &[
    "embed",
    "ocr",
    "moderation",
    "rerank",
    "guard",
    "tts",
    "stt",
    "whisper",
    "transcribe",
    "speech",
    "image",
    "video",
];

/// Published capabilities outrank the name heuristic.
fn can_carry_a_turn(id: &str, declares_chat: Option<bool>) -> bool {
    if let Some(declared) = declares_chat {
        return declared;
    }
    let id = id.to_ascii_lowercase();
    !NON_CONVERSATIONAL_MARKERS
        .iter()
        .any(|marker| id.contains(marker))
}

/// Fetch declared metadata from a compatible `GET /models` endpoint.
///
/// Discovery carries no vendor-specific fallbacks or native tool assumptions.
/// Unknown capability fields default to false, and token limits to `None`.
async fn fetch_models(
    base_url: &str,
    token: Option<&str>,
    egress: Option<chaos_client::Egress>,
    headers: HeaderMap,
) -> Result<Vec<chaos_abi::AbiModelInfo>, chaos_abi::ListModelsError> {
    use rama::Service;
    use rama::http::Body;
    use rama::http::Request;
    use rama::http::StatusCode;
    use rama::http::body::util::BodyExt;

    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut builder = Request::builder().method("GET").uri(url.as_str());
    if let Some(token) = token {
        let bearer = format!("Bearer {token}");
        builder = builder.header(rama::http::header::AUTHORIZATION, bearer);
    }
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(Body::empty())
        .map_err(|e| chaos_abi::ListModelsError::Failed {
            message: e.to_string(),
        })?;

    let client = chaos_client::default_rama_http_client_with_egress(egress);
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
    models_from_slice(&body)
}

fn models_from_slice(
    body: &[u8],
) -> Result<Vec<chaos_abi::AbiModelInfo>, chaos_abi::ListModelsError> {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct ModelsListResponse {
        data: Vec<ModelEntry>,
    }

    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
        #[serde(default, alias = "name")]
        display_name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(
            default,
            alias = "context_window",
            alias = "max_context_tokens",
            alias = "max_context_length",
            alias = "max_input_tokens"
        )]
        context_length: Option<i64>,
        #[serde(default, alias = "max_output_tokens")]
        max_tokens_output: Option<i64>,
        #[serde(default, alias = "supports_thinking")]
        supports_reasoning: Option<bool>,
        #[serde(default, alias = "supports_vision", alias = "supports_image_input")]
        supports_image_in: Option<bool>,
        #[serde(default)]
        capabilities: Option<ModelCapabilities>,
    }

    /// A published capability block outranks top-level hints.
    #[derive(Deserialize, Default)]
    struct ModelCapabilities {
        #[serde(default, alias = "chat", alias = "chat_completion")]
        completion_chat: Option<bool>,
        #[serde(default, alias = "image_input")]
        vision: Option<bool>,
        #[serde(default, alias = "thinking")]
        reasoning: Option<bool>,
    }

    let resp: ModelsListResponse =
        serde_json::from_slice(body).map_err(|e| chaos_abi::ListModelsError::Failed {
            message: format!("parse: {e}"),
        })?;

    Ok(resp
        .data
        .into_iter()
        .filter(|m| {
            can_carry_a_turn(
                &m.id,
                m.capabilities
                    .as_ref()
                    .and_then(|caps| caps.completion_chat),
            )
        })
        .map(|m| {
            let id = m.id;
            let caps = m.capabilities.unwrap_or_default();
            chaos_abi::AbiModelInfo {
                display_name: m.display_name.unwrap_or_else(|| id.clone()),
                id,
                model_family: chaos_ipc::openai_models::ModelFamily::default(),
                description: m.description,
                max_input_tokens: m.context_length,
                max_output_tokens: m.max_tokens_output,
                supports_thinking: caps.reasoning.or(m.supports_reasoning).unwrap_or(false),
                supports_images: caps.vision.or(m.supports_image_in).unwrap_or(false),
                supports_structured_output: false,
                supports_reasoning_effort: false,
                native_server_side_tools: vec![],
            }
        })
        .collect())
}

#[cfg(test)]
mod tests;
