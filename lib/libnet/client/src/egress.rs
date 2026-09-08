//! Provider-neutral routing through LSD's dynamic-upstream egress lane.
//!
//! Apply after provider authentication and before HTTP dispatch. The provider
//! URL remains the source of truth for wire-format selection and cookie scope.

use rama::Service;
use rama::error::ErrorExt;
use rama::error::extra::OpaqueError;
use rama::http::{Request, Response};
use rama::service::BoxService;
use url::Url;

pub const EGRESS_UPSTREAM_HEADER: &str = "x-lsd-upstream";

/// A validated egress endpoint, e.g. `http://gateway:3000/egress/chaos`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Egress {
    endpoint: String,
}

impl Egress {
    pub fn parse(endpoint: &str) -> Result<Self, String> {
        let url = Url::parse(endpoint).map_err(|err| format!("invalid egress_url: {err}"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "egress_url must be an HTTP(S) endpoint without credentials, query, or fragment"
                    .to_string(),
            );
        }
        Ok(Self {
            endpoint: url.as_str().trim_end_matches('/').to_string(),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Preserve the original path/query, body, and vendor headers. The gateway
    /// consumes the upstream origin header; callers cannot override routing.
    /// Incompatible upstreams are left for LSD's SSRF policy to reject, never
    /// silently sent direct.
    pub fn route<B>(&self, request: &mut Request<B>) -> Result<(), OpaqueError> {
        let uri = request.uri();
        let scheme = uri.scheme().ok_or_else(|| {
            OpaqueError::from_static_str("egress requires an absolute upstream URL")
        })?;
        let authority = uri
            .authority()
            .ok_or_else(|| OpaqueError::from_static_str("egress requires an upstream authority"))?;
        let origin = format!("{scheme}://{authority}");
        let target = format!("{}{}", self.endpoint, uri.request_target());
        let target = target.parse().map_err(ErrorExt::into_opaque_error)?;
        let origin = origin.parse().map_err(ErrorExt::into_opaque_error)?;
        *request.uri_mut() = target;
        request.headers_mut().remove(rama::http::header::HOST);
        request.headers_mut().insert(EGRESS_UPSTREAM_HEADER, origin);
        // This is a new HTTP hop. Inbound clamp requests can carry resolved
        // CONNECT targets, DNS candidates, or TLS connection state that Rama
        // prefers over the URI. Do not carry any of that routing state into
        // the gateway connection (trace/auth information is in the headers).
        request.set_extensions(Default::default());
        Ok(())
    }
}

struct EgressService {
    inner: BoxService<Request, Response, OpaqueError>,
    egress: Egress,
}

impl Service<Request> for EgressService {
    type Output = Response;
    type Error = OpaqueError;

    async fn serve(&self, mut request: Request) -> Result<Response, OpaqueError> {
        self.egress.route(&mut request)?;
        self.inner.serve(request).await
    }
}

pub(crate) fn with_egress(
    client: BoxService<Request, Response, OpaqueError>,
    egress: Option<Egress>,
) -> BoxService<Request, Response, OpaqueError> {
    match egress {
        Some(egress) => EgressService {
            inner: client,
            egress,
        }
        .boxed(),
        None => client,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rama::extensions::ExtensionsRef;
    use rama::http::Body;

    #[test]
    fn routes_every_vendor_without_changing_payload_or_credentials() {
        let egress = Egress::parse("http://gateway:3000/egress/chaos/").unwrap();
        for upstream in [
            "https://api.openai.com/v1/responses",
            "https://chatgpt.com/backend-api/codex/responses",
            "https://cli-chat-proxy.grok.com/v1/responses",
            "https://api.anthropic.com/v1/messages?beta=true",
            "https://api.minimax.io/anthropic/v1/messages",
            "https://deployment.openai.azure.com/openai/responses?api-version=2025-03-01",
            "https://custom.example/v1/files/a%2Fb?key=a%2Bb",
            "http://localhost:11434/v1/chat/completions",
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri(upstream)
                .header("authorization", "Bearer vendor-token")
                .header("x-api-key", "vendor-key")
                .header("host", "stale.example")
                .header(EGRESS_UPSTREAM_HEADER, "https://spoofed.example")
                .body("unchanged bytes")
                .unwrap();
            let target = request.uri().request_target().into_owned();
            let url = Url::parse(upstream).unwrap();
            request
                .extensions()
                .insert(rama::net::client::ConnectorTarget(
                    "vendor.example:443".parse().unwrap(),
                ));
            egress.route(&mut request).unwrap();
            assert!(
                !request
                    .extensions()
                    .contains::<rama::net::client::ConnectorTarget>()
            );
            assert_eq!(
                request.uri().to_string(),
                format!("http://gateway:3000/egress/chaos{target}")
            );
            assert_eq!(
                request.headers()[EGRESS_UPSTREAM_HEADER],
                url.origin().ascii_serialization()
            );
            assert_eq!(request.headers()["authorization"], "Bearer vendor-token");
            assert_eq!(request.headers()["x-api-key"], "vendor-key");
            assert!(!request.headers().contains_key("host"));
            assert_eq!(request.method(), "POST");
            assert_eq!(*request.body(), "unchanged bytes");
        }
    }

    #[test]
    fn rejects_invalid_gateway_configuration() {
        for endpoint in [
            "",
            "gateway:3000/egress/chaos",
            "ftp://gateway/egress/chaos",
            "https://user:password@gateway/egress/chaos",
            "https://gateway/egress/chaos?token=secret",
            "https://gateway/egress/chaos#fragment",
        ] {
            assert!(Egress::parse(endpoint).is_err(), "{endpoint}");
        }
    }

    #[tokio::test]
    async fn gateway_failure_is_returned_without_direct_fallback() {
        let client = rama::service::service_fn(|request: Request| async move {
            assert_eq!(
                request.uri().to_string(),
                "http://gateway/egress/chaos/v1/responses"
            );
            Err::<Response, _>(OpaqueError::from_static_str("gateway unavailable"))
        })
        .boxed();
        let client = with_egress(
            client,
            Some(Egress::parse("http://gateway/egress/chaos").unwrap()),
        );
        let request = Request::builder()
            .uri("https://api.openai.com/v1/responses")
            .body(Body::empty())
            .unwrap();
        assert!(
            client
                .serve(request)
                .await
                .unwrap_err()
                .to_string()
                .contains("gateway unavailable")
        );
    }
}
