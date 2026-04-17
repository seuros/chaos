//! xAI Batch API — inline-request lifecycle.
//!
//! <https://docs.x.ai/docs/advanced-api-usage/batch-api>

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

use crate::chat_completions::build_request_body;

const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_BATCH_NAME: &str = "chaos-spool";
const RESULTS_PAGE_SIZE: u32 = 100;

pub struct XaiSpoolBackend {
    base_url: String,
    api_key: Secret<String>,
    default_model: String,
    client: ChaosHttpClient,
}

impl XaiSpoolBackend {
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
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    fn bearer(&self) -> Result<HeaderValue, SpoolError> {
        HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose()))
            .map_err(|e| SpoolError::Other(format!("invalid api key header: {e}")))
    }

    fn model_for(&self, req: &TurnRequest) -> String {
        if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        }
    }

    async fn get_json(&self, url: &str) -> Result<Value, SpoolError> {
        let auth = self.bearer()?;
        let resp = self
            .client
            .get(url)
            .header("authorization", auth)
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

    async fn post_json(&self, url: &str, body: Value) -> Result<Value, SpoolError> {
        let auth = self.bearer()?;
        let resp = self
            .client
            .post(url)
            .header("authorization", auth)
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
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .map_err(|e| SpoolError::Translation(format!("decode: {e}; body={text}")))
    }
}

#[derive(Debug, Deserialize)]
struct CreateBatchResponse {
    batch_id: String,
}

#[derive(Debug, Deserialize)]
struct BatchState {
    #[serde(default)]
    num_requests: u32,
    #[serde(default)]
    num_pending: u32,
    #[serde(default)]
    num_success: u32,
    #[serde(default)]
    num_error: u32,
    #[serde(default)]
    num_cancelled: u32,
}

#[derive(Debug, Deserialize)]
struct GetBatchResponse {
    #[serde(default)]
    state: Option<BatchState>,
    #[serde(default)]
    cancelled_at: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ResultsPage {
    #[serde(default)]
    results: Vec<ResultEntry>,
    #[serde(default)]
    pagination_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResultEntry {
    #[serde(default)]
    batch_request_id: String,
    #[serde(default)]
    batch_result: Option<Value>,
    #[serde(default)]
    error_message: Option<String>,
}

impl SpoolBackend for XaiSpoolBackend {
    fn name(&self) -> &'static str {
        "xai"
    }

    fn submit(
        &self,
        items: Vec<(String, TurnRequest)>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
        Box::pin(async move {
            let create_url = self.url("/batches");
            let created: CreateBatchResponse = serde_json::from_value(
                self.post_json(&create_url, json!({ "name": DEFAULT_BATCH_NAME }))
                    .await?,
            )
            .map_err(|e| SpoolError::Translation(format!("decode create batch: {e}")))?;

            let mut batch_requests = Vec::with_capacity(items.len());
            for (custom_id, req) in items {
                let model = self.model_for(&req);
                let mut body = build_request_body(&req, &model)
                    .map_err(|e| SpoolError::Translation(e.to_string()))?;
                if let Some(obj) = body.as_object_mut() {
                    obj.remove("stream");
                    obj.remove("stream_options");
                }
                batch_requests.push(json!({
                    "batch_request_id": custom_id,
                    "batch_request": { "chat_get_completion": body },
                }));
            }

            let add_url = self.url(&format!("/batches/{}/requests", created.batch_id));
            self.post_json(&add_url, json!({ "batch_requests": batch_requests }))
                .await?;
            Ok(created.batch_id)
        })
    }

    fn poll(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>> {
        let url = self.url(&format!("/batches/{batch_id}"));
        Box::pin(async move {
            let resp: GetBatchResponse = serde_json::from_value(self.get_json(&url).await?)
                .map_err(|e| SpoolError::Translation(format!("decode poll: {e}")))?;
            let state = resp
                .state
                .ok_or_else(|| SpoolError::Translation("batch response missing state".into()))?;
            let cancelled = resp.cancelled_at.is_some();
            let raw = format!(
                "pending={} success={} error={} cancelled={}",
                state.num_pending, state.num_success, state.num_error, state.num_cancelled,
            );
            let phase = if cancelled && state.num_pending == 0 {
                SpoolPhase::Cancelled
            } else if state.num_requests > 0 && state.num_pending == 0 {
                if state.num_success == 0 && state.num_error > 0 {
                    SpoolPhase::Failed
                } else {
                    SpoolPhase::Completed
                }
            } else {
                SpoolPhase::InProgress
            };
            Ok(SpoolStatusReport {
                phase,
                raw_provider_status: raw,
            })
        })
    }

    fn fetch_results(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
        let base = self.url(&format!("/batches/{batch_id}/results"));
        Box::pin(async move {
            let mut out = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let url = match &token {
                    Some(t) => format!(
                        "{base}?limit={RESULTS_PAGE_SIZE}&pagination_token={}",
                        urlencode(t)
                    ),
                    None => format!("{base}?limit={RESULTS_PAGE_SIZE}"),
                };
                let page: ResultsPage = serde_json::from_value(self.get_json(&url).await?)
                    .map_err(|e| SpoolError::Translation(format!("decode results page: {e}")))?;
                for entry in page.results {
                    out.push((entry.batch_request_id.clone(), decode_entry(&entry)?));
                }
                match page.pagination_token {
                    Some(t) if !t.is_empty() => token = Some(t),
                    _ => break,
                }
            }
            Ok(out)
        })
    }

    fn cancel(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
        let url = self.url(&format!("/batches/{batch_id}:cancel"));
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

fn decode_entry(entry: &ResultEntry) -> Result<TurnResult, SpoolError> {
    if let Some(result) = &entry.batch_result {
        let response = result.get("response");
        if let Some(chat) = response.and_then(|r| r.get("chat_get_completion")) {
            let choice = chat
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first());
            let message = choice.and_then(|c| c.get("message"));
            let text = message
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let finish_reason = choice
                .and_then(|c| c.get("finish_reason"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let server_model = chat.get("model").and_then(|s| s.as_str()).map(String::from);
            let content = if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentItem::OutputText { text }]
            };
            return Ok(TurnResult::Success(TurnOutput {
                content,
                finish_reason,
                usage: None,
                server_model,
            }));
        }
    }
    let message = entry
        .error_message
        .clone()
        .unwrap_or_else(|| "unknown xai batch error".to_string());
    Ok(TurnResult::Error(TurnError {
        code: "xai_batch_error".into(),
        message,
        usage: None,
    }))
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
