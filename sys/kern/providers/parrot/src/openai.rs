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
        Self {
            client: ResponsesClient::new(transport, provider, auth),
            options: ResponsesOptions::default(),
            default_model,
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
