use crate::auth::AuthProvider;
use crate::common::MemorySummarizeInput;
use crate::common::MemorySummarizeOutput;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use chaos_client::HttpTransport;
use chaos_client::RequestTelemetry;
use rama::http::HeaderMap;
use serde::Deserialize;
use std::sync::Arc;

pub struct MemoriesClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> MemoriesClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    fn path() -> &'static str {
        "memories/trace_summarize"
    }

    pub async fn summarize(
        &self,
        body: serde_json::Value,
        extra_headers: HeaderMap,
    ) -> Result<Vec<MemorySummarizeOutput>, ApiError> {
        let parsed: SummarizeResponse = self
            .session
            .post_json_response(
                Self::path(),
                extra_headers,
                body,
                "memory summarize response",
            )
            .await?;
        Ok(parsed.output)
    }

    pub async fn summarize_input(
        &self,
        input: &MemorySummarizeInput,
        extra_headers: HeaderMap,
    ) -> Result<Vec<MemorySummarizeOutput>, ApiError> {
        let parsed: SummarizeResponse = self
            .session
            .post_json_input(
                Self::path(),
                input,
                extra_headers,
                "memory summarize input",
                "memory summarize response",
            )
            .await?;
        Ok(parsed.output)
    }
}

#[derive(Debug, Deserialize)]
struct SummarizeResponse {
    output: Vec<MemorySummarizeOutput>,
}

#[cfg(test)]
mod tests;
