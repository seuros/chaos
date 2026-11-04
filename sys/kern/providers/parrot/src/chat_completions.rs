//! Chat Completions API adapter.
//!
//! Translates chaos-abi `TurnRequest` into OpenAI's `/v1/chat/completions`
//! wire format and streams `TurnEvent`s back from the response.
//!
//! HTTP/SSE only — no WebSocket, no sticky routing, no incremental request
//! reuse.  Each follow-up sends full conversation history.

#![warn(clippy::all)]

use crate::provider::Provider;
use chaos_abi::AbiError;
use chaos_abi::AdapterFuture;
use chaos_abi::ModelAdapter;
use chaos_abi::TurnRequest;
use chaos_abi::TurnStream;
use chaos_libration::UsageSniffer;
use rama::http::HeaderMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Adapter for the OpenAI Chat Completions API (`/v1/chat/completions`).
///
/// Holds a provider (headers, base URL, retry config) and an optional API key.
#[derive(Debug, Clone)]
pub struct ChatCompletionsAdapter {
    provider: Provider,
    api_key: Option<String>,
    default_model: Option<String>,
    sniffer: Option<Arc<UsageSniffer>>,
}

impl ChatCompletionsAdapter {
    /// Create an adapter backed by a fully-configured provider.
    pub fn new(provider: Provider, api_key: Option<String>, default_model: Option<String>) -> Self {
        Self {
            provider,
            api_key,
            default_model,
            sniffer: None,
        }
    }

    /// Attach an optional ration sniffer that records rate-limit
    /// headers for every response this adapter issues.
    pub fn with_sniffer(mut self, sniffer: Option<Arc<UsageSniffer>>) -> Self {
        self.sniffer = sniffer;
        self
    }

    /// Convenience constructor for standalone use (tests, `adapter_for_wire`).
    pub fn from_base_url_and_api_key(
        base_url: String,
        api_key: String,
        default_model: Option<String>,
    ) -> Self {
        let provider = Provider::from_base_url_with_default_streaming_config(
            "ChatCompletions",
            base_url,
            false,
        );
        Self::new(provider, Some(api_key), default_model)
    }

    fn chat_completions_url(&self) -> String {
        self.provider.url_for_path("/chat/completions")
    }

    fn model_for_request(&self, request_model: &str) -> Result<String, AbiError> {
        if !request_model.is_empty() {
            return Ok(request_model.to_string());
        }
        self.default_model
            .clone()
            .ok_or_else(|| AbiError::InvalidRequest {
                message: "no model configured for Chat Completions provider".to_string(),
            })
    }

    fn build_headers(&self, stream: bool) -> Result<HeaderMap, AbiError> {
        let mut headers = self.provider.headers.clone();
        if let Some(api_key) = &self.api_key {
            crate::http_helpers::insert_bearer_auth(&mut headers, api_key, "Chat Completions")?;
        }
        if stream {
            crate::http_helpers::insert_streaming_json_headers(&mut headers);
        } else {
            crate::http_helpers::insert_json_headers(&mut headers);
        }
        Ok(headers)
    }
}

impl ModelAdapter for ChatCompletionsAdapter {
    fn stream(&self, request: TurnRequest) -> AdapterFuture<'_> {
        Box::pin(async move {
            let url = self.chat_completions_url();
            let model = self.model_for_request(&request.model)?;
            let structured = request.output_schema.is_some();
            let body = build_request_body(&request, &model, !structured)?;
            let headers = self.build_headers(!structured)?;
            let provider = self.provider.clone();
            let sniffer = self.sniffer.clone();

            let (tx, rx) = mpsc::channel(64);

            tokio::spawn(async move {
                let result = transport::run_response(
                    &url,
                    &headers,
                    &body,
                    &provider,
                    sniffer.as_ref(),
                    tx.clone(),
                )
                .await;
                if let Err(e) = result {
                    // A disconnected consumer no longer needs the terminal error.
                    let _ = tx.send(Err(e)).await;
                }
            });

            Ok(TurnStream { rx_event: rx })
        })
    }

    fn provider_name(&self) -> &str {
        "ChatCompletions"
    }

    fn capabilities(&self) -> chaos_abi::AdapterCapabilities {
        chaos_abi::AdapterCapabilities {
            can_list_models: false,
        }
    }
}

mod parse;
mod request;
mod transport;

pub(crate) use request::build_request_body;

#[cfg(test)]
pub(crate) use parse::parse_chunk;
#[cfg(test)]
pub(crate) use transport::parse_completion;
#[cfg(test)]
pub(crate) use transport::process_json_response;
#[cfg(test)]
pub(crate) use transport::process_sse_data_stream;

#[cfg(test)]
mod tests;
