use super::*;
use crate::provider::RetryConfig;
use chaos_client::Request;
use chaos_client::Response;
use chaos_client::StreamResponse;
use chaos_client::TransportError;
use chaos_test_fixtures::TEST_MODEL;
use pretty_assertions::assert_eq;
use rama::http::HeaderMap;
use rama::http::StatusCode;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone)]
struct CapturingTransport {
    last_request: Arc<Mutex<Option<Request>>>,
    body: Arc<ModelsResponse>,
    etag: Option<String>,
}

impl Default for CapturingTransport {
    fn default() -> Self {
        Self {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(ModelsResponse { models: Vec::new() }),
            etag: None,
        }
    }
}

impl HttpTransport for CapturingTransport {
    async fn execute(&self, req: Request) -> Result<Response, TransportError> {
        *self.last_request.lock().unwrap() = Some(req);
        let body = serde_json::to_vec(&*self.body).unwrap();
        let mut headers = HeaderMap::new();
        if let Some(etag) = &self.etag {
            headers.insert(ETAG, etag.parse().unwrap());
        }
        Ok(Response {
            status: StatusCode::OK,
            headers,
            body: body.into(),
        })
    }

    async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
        Err(TransportError::Build("stream should not run".to_string()))
    }
}

#[derive(Clone, Default)]
struct DummyAuth;

impl AuthProvider for DummyAuth {
    fn bearer_token(&self) -> Option<String> {
        None
    }
}

fn provider(base_url: &str) -> Provider {
    Provider {
        egress: None,
        name: "test".to_string(),
        base_url: base_url.to_string(),
        query_params: None,
        headers: HeaderMap::new(),
        retry: RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        },
        stream_idle_timeout: Duration::from_secs(1),
    }
}

#[tokio::test]
async fn appends_client_version_query() {
    let response = ModelsResponse { models: Vec::new() };

    let transport = CapturingTransport {
        last_request: Arc::new(Mutex::new(None)),
        body: Arc::new(response),
        etag: None,
    };

    let client = ModelsClient::new(
        transport.clone(),
        provider("https://example.com/api/chaos"),
        DummyAuth,
    );

    let (models, _) = client
        .list_models("0.99.0", HeaderMap::new())
        .await
        .expect("request should succeed");

    assert_eq!(models.len(), 0);

    let url = transport
        .last_request
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .url
        .clone();
    assert_eq!(
        url,
        "https://example.com/api/chaos/models?client_version=0.99.0"
    );
}

#[tokio::test]
async fn parses_models_response() {
    let response = ModelsResponse {
            models: vec![
                serde_json::from_value(json!({
                    "slug": TEST_MODEL,
                    "display_name": TEST_MODEL,
                    "description": "desc",
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}, {"effort": "high", "description": "high"}],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "minimal_client_version": [0, 99, 0],
                    "supported_in_api": true,
                    "priority": 1,
                    "upgrade": null,
                    "base_instructions": "base instructions",
                    "supports_reasoning_summaries": false,
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "truncation_policy": {"mode": "bytes", "limit": 10_000},
                    "supports_parallel_tool_calls": false,
                    "supports_image_detail_original": false,
                    "context_window": 272_000,
                    "experimental_supported_tools": [],
                }))
                .unwrap(),
            ],
        };

    let transport = CapturingTransport {
        last_request: Arc::new(Mutex::new(None)),
        body: Arc::new(response),
        etag: None,
    };

    let client = ModelsClient::new(
        transport,
        provider("https://example.com/api/chaos"),
        DummyAuth,
    );

    let (models, _) = client
        .list_models("0.99.0", HeaderMap::new())
        .await
        .expect("request should succeed");

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].slug, TEST_MODEL);
    assert_eq!(models[0].supported_in_api, true);
    assert_eq!(models[0].priority, 1);
}

#[tokio::test]
async fn list_models_includes_etag() {
    let response = ModelsResponse { models: Vec::new() };

    let transport = CapturingTransport {
        last_request: Arc::new(Mutex::new(None)),
        body: Arc::new(response),
        etag: Some("\"abc\"".to_string()),
    };

    let client = ModelsClient::new(
        transport,
        provider("https://example.com/api/chaos"),
        DummyAuth,
    );

    let (models, etag) = client
        .list_models("0.1.0", HeaderMap::new())
        .await
        .expect("request should succeed");

    assert_eq!(models.len(), 0);
    assert_eq!(etag, Some("\"abc\"".to_string()));
}
