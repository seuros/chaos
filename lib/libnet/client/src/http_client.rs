use rama::Service;
use rama::error::extra::OpaqueError;
use rama::http::{Request, Response};
use rama::service::BoxService;

use crate::Egress;

pub(crate) type RamaClient = BoxService<Request, Response, OpaqueError>;

pub(crate) fn raw_http_client() -> RamaClient {
    crate::ensure_rustls_crypto_provider();
    rama::http::client::EasyHttpWebClient::default().boxed()
}

pub(crate) fn with_http_policies(client: RamaClient, egress: Option<Egress>) -> RamaClient {
    // Cookies must see the vendor origin, not the shared gateway host.
    crate::infrastructure_cookies::with_infrastructure_cookies(crate::egress::with_egress(
        client, egress,
    ))
}

pub fn default_rama_http_client() -> RamaClient {
    default_rama_http_client_with_egress(None)
}

pub fn default_rama_http_client_with_egress(egress: Option<Egress>) -> RamaClient {
    with_http_policies(raw_http_client(), egress)
}
