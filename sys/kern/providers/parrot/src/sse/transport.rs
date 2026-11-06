use crate::provider::Provider;
use chaos_abi::AbiError;
use chaos_client::TransportError;
use chaos_client::default_rama_http_client_with_egress;
use chaos_client::run_with_retry;
use chaos_libration::UsageSniffer;
use rama::Service;
use rama::bytes::Bytes;
use rama::http::Body;
use rama::http::HeaderMap;
use rama::http::HttpResult;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::body::util::BodyExt;
use rama::telemetry::tracing;
use serde_json::Value;
use std::sync::Arc;

/// Execute a POST SSE request with the provider's retry policy and return the successful response.
///
/// When `sniffer` is `Some`, rate-limit headers from the response are handed
/// to the ration pipeline — regardless of status, because 429/5xx responses
/// carry the freshest reset hints. When `None`, no recording happens; the
/// caller is expected to have attached a sniffer at adapter construction
/// time if it wanted one.
pub(crate) async fn start_rama_post_sse_request(
    url: &str,
    headers: &HeaderMap,
    body: &Value,
    provider: &Provider,
    transport_name: &str,
    sniffer: Option<&Arc<UsageSniffer>>,
) -> Result<Response<Body>, AbiError> {
    let body_bytes = encode_body(body)?;

    let make_request = || -> HttpResult<Request<Body>> {
        let mut builder = Request::builder().method("POST").uri(url);
        for (name, value) in headers.iter() {
            builder = builder.header(name, value);
        }
        builder.body(Body::from(body_bytes.clone()))
    };

    run_with_retry(provider.retry.to_policy(), make_request, |request, attempt| async move {
        let request = request.map_err(|e| TransportError::Build(e.to_string()))?;
        let client = default_rama_http_client_with_egress(provider.egress.clone());
        let response = client.serve(request).await.map_err(|err| {
            tracing::warn!(attempt, transport = transport_name, "SSE transport error: {err}");
            TransportError::Network(err.to_string())
        })?;

        let status = response.status();
        if let Some(sniffer) = sniffer {
            sniffer.sniff(response.headers());
        }
        if status == StatusCode::OK {
            return Ok(response);
        }

        tracing::warn!(attempt, transport = transport_name, status = %status, "SSE request failed");
        let err_body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| TransportError::Http {
                status,
                url: None,
                headers: None,
                body: Some(e.to_string()),
            })?
            .to_bytes();
        Err(TransportError::Http {
            status,
            url: None,
            headers: None,
            body: Some(String::from_utf8_lossy(&err_body).into_owned()),
        })
    })
    .await
    .map_err(|err| match err {
        TransportError::Build(message) => AbiError::InvalidRequest { message },
        TransportError::Http { status, body, .. } => AbiError::Transport {
            status: status.as_u16(),
            message: body.unwrap_or_default(),
        },
        other => AbiError::Transport {
            status: 0,
            message: other.to_string(),
        },
    })
}

fn encode_body(body: &Value) -> Result<Bytes, AbiError> {
    serde_json::to_vec(body)
        .map(Bytes::from)
        .map_err(|e| AbiError::InvalidRequest {
            message: e.to_string(),
        })
}

#[cfg(test)]
mod tests;
