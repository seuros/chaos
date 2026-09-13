use crate::auth::AuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use chaos_client::HttpTransport;
use chaos_client::RequestTelemetry;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelsResponse;
use rama::http::HeaderMap;
use rama::http::Method;
use rama::http::header::ETAG;
use std::sync::Arc;

pub struct ModelsClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> ModelsClient<T, A> {
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
        "models"
    }

    fn append_client_version_query(req: &mut chaos_client::Request, client_version: &str) {
        let separator = if req.url.contains('?') { '&' } else { '?' };
        req.url = format!("{}{}client_version={client_version}", req.url, separator);
    }

    pub async fn list_models(
        &self,
        client_version: &str,
        extra_headers: HeaderMap,
    ) -> Result<(Vec<ModelInfo>, Option<String>), ApiError> {
        let resp = self
            .session
            .execute_with(
                Method::GET,
                Self::path(),
                extra_headers,
                /*body*/ None,
                |req| {
                    Self::append_client_version_query(req, client_version);
                },
            )
            .await?;

        let header_etag = resp
            .headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);

        let ModelsResponse { models } = serde_json::from_slice::<ModelsResponse>(&resp.body)
            .map_err(|e| {
                ApiError::Stream(format!(
                    "failed to decode models response: {e}; body: {}",
                    String::from_utf8_lossy(&resp.body)
                ))
            })?;

        Ok((models, header_etag))
    }
}

#[cfg(test)]
mod tests;
