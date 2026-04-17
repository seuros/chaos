//! Anthropic Message Batches — inline-request lifecycle.
//!
//! <https://docs.anthropic.com/en/api/creating-message-batches>

use std::future::Future;
use std::pin::Pin;

use chaos_abi::ContentItem;
use chaos_abi::Secret;
use chaos_abi::SpoolBackend;
use chaos_abi::SpoolError;
use chaos_abi::SpoolItem;
use chaos_abi::SpoolPhase;
use chaos_abi::SpoolStatusReport;
use chaos_abi::TurnRequest;
use chaos_abi::TurnResult;
use chaos_abi::turn_result::TurnError;
use chaos_abi::turn_result::TurnOutput;
use codex_client::ChaosHttpClient;
use rama::http::HeaderValue;
use rama::http::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::anthropic::build_request_body;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

pub struct AnthropicSpoolBackend {
    base_url: String,
    api_key: Secret<String>,
    default_model: String,
    client: ChaosHttpClient,
}

impl AnthropicSpoolBackend {
    pub fn new(api_key: String, default_model: String) -> Self {
        Self::with_base_url(api_key, default_model, DEFAULT_BASE_URL.to_string())
    }

    pub fn with_base_url(api_key: String, default_model: String, base_url: String) -> Self {
        Self {
            base_url,
            api_key: Secret::new(api_key),
            default_model,
            client: ChaosHttpClient::default_client(),
        }
    }

    pub fn with_http_client(mut self, client: ChaosHttpClient) -> Self {
        self.client = client;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path,)
    }

    fn model_for(&self, req: &TurnRequest) -> String {
        if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        }
    }

    async fn get_json(&self, url: &str) -> Result<Value, SpoolError> {
        let key = HeaderValue::from_str(self.api_key.expose())
            .map_err(|e| SpoolError::Other(format!("invalid api key header: {e}")))?;
        let resp = self
            .client
            .get(url)
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| SpoolError::Other(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| SpoolError::Other(format!("read body: {e}")))?;
        classify_status(status, &body)?;
        serde_json::from_str(&body)
            .map_err(|e| SpoolError::Translation(format!("decode: {e}; body={body}")))
    }

    async fn get_text(&self, url: &str) -> Result<String, SpoolError> {
        let key = HeaderValue::from_str(self.api_key.expose())
            .map_err(|e| SpoolError::Other(format!("invalid api key header: {e}")))?;
        let resp = self
            .client
            .get(url)
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await
            .map_err(|e| SpoolError::Other(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| SpoolError::Other(format!("read body: {e}")))?;
        classify_status(status, &body)?;
        Ok(body)
    }

    async fn post_json(&self, url: &str, body: Value) -> Result<Value, SpoolError> {
        let key = HeaderValue::from_str(self.api_key.expose())
            .map_err(|e| SpoolError::Other(format!("invalid api key header: {e}")))?;
        let resp = self
            .client
            .post(url)
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| SpoolError::Other(format!("POST {url}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| SpoolError::Other(format!("read body: {e}")))?;
        classify_status(status, &text)?;
        serde_json::from_str(&text)
            .map_err(|e| SpoolError::Translation(format!("decode: {e}; body={text}")))
    }
}

#[derive(Debug, Deserialize)]
struct BatchEnvelope {
    id: String,
    processing_status: String,
    #[serde(default)]
    results_url: Option<String>,
}

impl SpoolBackend for AnthropicSpoolBackend {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn submit(
        &self,
        items: Vec<(String, TurnRequest)>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
        Box::pin(async move {
            let mut requests = Vec::with_capacity(items.len());
            for (custom_id, req) in items {
                let model = self.model_for(&req);
                let mut body = build_request_body(&req, &model)
                    .map_err(|e| SpoolError::Translation(e.to_string()))?;
                if let Some(obj) = body.as_object_mut() {
                    obj.remove("stream");
                }
                requests.push(json!({ "custom_id": custom_id, "params": body }));
            }
            let payload = json!({ "requests": requests });
            let url = self.url("/messages/batches");
            let env: BatchEnvelope =
                serde_json::from_value(self.post_json(&url, payload).await?)
                    .map_err(|e| SpoolError::Translation(format!("decode submit envelope: {e}")))?;
            Ok(env.id)
        })
    }

    fn poll(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>> {
        let url = self.url(&format!("/messages/batches/{batch_id}"));
        Box::pin(async move {
            let env: BatchEnvelope = serde_json::from_value(self.get_json(&url).await?)
                .map_err(|e| SpoolError::Translation(format!("decode poll envelope: {e}")))?;
            let phase = match env.processing_status.as_str() {
                "in_progress" => SpoolPhase::InProgress,
                "ended" => SpoolPhase::Completed,
                "canceling" | "canceled" => SpoolPhase::Cancelled,
                other => {
                    return Err(SpoolError::Other(format!(
                        "unexpected anthropic status: {other}"
                    )));
                }
            };
            Ok(SpoolStatusReport {
                phase,
                raw_provider_status: env.processing_status,
            })
        })
    }

    fn fetch_results(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
        let envelope_url = self.url(&format!("/messages/batches/{batch_id}"));
        Box::pin(async move {
            let env: BatchEnvelope = serde_json::from_value(self.get_json(&envelope_url).await?)
                .map_err(|e| SpoolError::Translation(format!("decode fetch envelope: {e}")))?;
            let results_url = env
                .results_url
                .ok_or_else(|| SpoolError::Other("batch ended but results_url missing".into()))?;
            let jsonl = self.get_text(&results_url).await?;

            let mut out = Vec::new();
            for line in jsonl.lines().filter(|l| !l.trim().is_empty()) {
                let v: Value = serde_json::from_str(line)
                    .map_err(|e| SpoolError::Translation(format!("malformed JSONL: {e}")))?;
                let custom_id = v
                    .get("custom_id")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_string();
                out.push((custom_id, decode_result_line(&v)?));
            }
            Ok(out)
        })
    }

    fn cancel(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
        let url = self.url(&format!("/messages/batches/{batch_id}/cancel"));
        Box::pin(async move {
            self.post_json(&url, json!({})).await?;
            Ok(())
        })
    }
}

fn classify_status(status: StatusCode, body: &str) -> Result<(), SpoolError> {
    if status.is_success() {
        return Ok(());
    }
    let code = status.as_u16();
    Err(match code {
        401 | 403 => SpoolError::Auth,
        429 => SpoolError::RateLimit { retry_after: None },
        _ => SpoolError::ProviderError {
            status: code,
            message: body.to_string(),
        },
    })
}

fn decode_result_line(v: &Value) -> Result<TurnResult, SpoolError> {
    let result_type = v
        .pointer("/result/type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| SpoolError::Translation("result.type missing".into()))?;
    match result_type {
        "succeeded" => {
            let message = v.pointer("/result/message").ok_or_else(|| {
                SpoolError::Translation("succeeded: result.message missing".into())
            })?;
            let content = message
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| {
                            b.get("text").and_then(|t| t.as_str()).map(|text| {
                                ContentItem::OutputText {
                                    text: text.to_string(),
                                }
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let finish_reason = message
                .get("stop_reason")
                .and_then(|s| s.as_str())
                .map(String::from);
            let server_model = message
                .get("model")
                .and_then(|s| s.as_str())
                .map(String::from);
            Ok(TurnResult::Success(TurnOutput {
                content,
                finish_reason,
                usage: None,
                server_model,
            }))
        }
        other => {
            let message = v
                .pointer("/result/error/message")
                .and_then(|s| s.as_str())
                .unwrap_or(other)
                .to_string();
            Ok(TurnResult::Error(TurnError {
                code: other.to_string(),
                message,
                usage: None,
            }))
        }
    }
}
