use crate::provider::RetryConfig;
use chaos_abi::AbiError;
use http::HeaderMap;
use rama::Service;
use rama::http::Body;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::body::util::BodyExt;
use rama::http::client::EasyHttpWebClient;
use serde_json::Value;

/// Execute a POST SSE request with the provider's retry policy and return the successful response.
pub(crate) async fn start_rama_post_sse_request(
    url: &str,
    headers: &HeaderMap,
    body: &Value,
    retry: &RetryConfig,
    transport_name: &str,
) -> Result<Response<Body>, AbiError> {
    let body_bytes = serde_json::to_vec(body).map_err(|e| AbiError::InvalidRequest {
        message: e.to_string(),
    })?;

    let max_attempts = retry.max_attempts.max(1);
    let base_delay = retry.base_delay;

    for attempt in 0..max_attempts {
        if attempt > 0 {
            let delay = base_delay * 2u32.saturating_pow(attempt.saturating_sub(1) as u32);
            tokio::time::sleep(delay).await;
        }

        let client = EasyHttpWebClient::default();
        let mut builder = Request::builder().method("POST").uri(url);
        for (name, value) in headers.iter() {
            builder = builder.header(name, value);
        }
        let request =
            builder
                .body(Body::from(body_bytes.clone()))
                .map_err(|e| AbiError::InvalidRequest {
                    message: e.to_string(),
                })?;

        let response = match client.serve(request).await {
            Ok(response) => response,
            Err(err) => {
                if retry.retry_transport && attempt + 1 < max_attempts {
                    tracing::warn!(
                        attempt,
                        transport = transport_name,
                        "SSE transport error, retrying: {err}"
                    );
                    continue;
                }
                return Err(AbiError::Transport {
                    status: 0,
                    message: err.to_string(),
                });
            }
        };

        let status = response.status();
        if status != StatusCode::OK {
            let err_body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| AbiError::Transport {
                    status: status.as_u16(),
                    message: e.to_string(),
                })?
                .to_bytes();
            let body_text = String::from_utf8_lossy(&err_body);

            let retryable = (status == StatusCode::TOO_MANY_REQUESTS && retry.retry_429)
                || (status.is_server_error() && retry.retry_5xx);
            if retryable && attempt + 1 < max_attempts {
                tracing::warn!(attempt, transport = transport_name, status = %status, "SSE retryable error, retrying");
                continue;
            }

            return Err(AbiError::Transport {
                status: status.as_u16(),
                message: body_text.to_string(),
            });
        }

        return Ok(response);
    }

    Err(AbiError::Transport {
        status: 0,
        message: "all retry attempts exhausted".to_string(),
    })
}
