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
mod tests;
