use rama::Service;
use rama::error::ErrorContext as _;
use rama::error::extra::OpaqueError;
use rama::extensions::ExtensionsRef as _;
use rama::http::Body;
use rama::http::Request;
use rama::http::Response;
use rama::http::client::EasyHttpWebClient;
use rama::net::address::ProxyAddress;
use rama::net::http::RequestContext;
use rama::rt::Executor;
use rama::service::BoxService;
use tracing::warn;

#[cfg(target_os = "macos")]
use rama::unix::client::UnixConnector;

#[derive(Clone, Default)]
struct ProxyConfig {
    http: Option<ProxyAddress>,
    https: Option<ProxyAddress>,
    all: Option<ProxyAddress>,
}

impl ProxyConfig {
    fn from_env() -> Self {
        let http = read_proxy_env(&["HTTP_PROXY", "http_proxy"]);
        let https = read_proxy_env(&["HTTPS_PROXY", "https_proxy"]);
        let all = read_proxy_env(&["ALL_PROXY", "all_proxy"]);
        Self { http, https, all }
    }

    fn proxy_for_request(&self, req: &Request) -> Option<ProxyAddress> {
        let is_secure = RequestContext::try_from(req)
            .map(|ctx| ctx.protocol.is_secure())
            .unwrap_or(false);
        self.proxy_for_protocol(is_secure)
    }

    fn proxy_for_protocol(&self, is_secure: bool) -> Option<ProxyAddress> {
        if is_secure {
            self.https
                .clone()
                .or_else(|| self.http.clone())
                .or_else(|| self.all.clone())
        } else {
            self.http.clone().or_else(|| self.all.clone())
        }
    }
}

fn read_proxy_env(keys: &[&str]) -> Option<ProxyAddress> {
    for key in keys {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match ProxyAddress::try_from(value) {
            Ok(proxy) => {
                if proxy
                    .protocol
                    .as_ref()
                    .map(rama::net::Protocol::is_http)
                    .unwrap_or(true)
                {
                    return Some(proxy);
                }
                warn!("ignoring {key}: non-http proxy protocol");
            }
            Err(err) => {
                warn!("ignoring {key}: invalid proxy address ({err})");
            }
        }
    }
    None
}

pub(crate) fn proxy_for_connect() -> Option<ProxyAddress> {
    ProxyConfig::from_env().proxy_for_protocol(/*is_secure*/ true)
}

type RamaHttpClient = BoxService<Request<Body>, Response, OpaqueError>;

#[derive(Clone)]
pub(crate) struct UpstreamClient {
    client: RamaHttpClient,
    proxy_config: ProxyConfig,
}

impl UpstreamClient {
    pub(crate) fn direct() -> Self {
        Self::new(ProxyConfig::default())
    }

    pub(crate) fn from_env_proxy() -> Self {
        Self::new(ProxyConfig::from_env())
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn unix_socket(path: &str) -> Self {
        let client = build_unix_client(path);
        Self {
            client,
            proxy_config: ProxyConfig::default(),
        }
    }

    fn new(proxy_config: ProxyConfig) -> Self {
        let client = build_http_client();
        Self {
            client,
            proxy_config,
        }
    }
}

impl Service<Request<Body>> for UpstreamClient {
    type Output = Response;
    type Error = OpaqueError;

    async fn serve(&self, req: Request<Body>) -> Result<Self::Output, Self::Error> {
        if let Some(proxy) = self.proxy_config.proxy_for_request(&req) {
            req.extensions().insert(proxy);
        }

        let uri = req.uri().clone();
        self.client
            .serve(req)
            .await
            .with_context(|| format!("http request failure for uri: {uri}"))
            .into_opaque_error()
    }
}

fn build_http_client() -> RamaHttpClient {
    EasyHttpWebClient::default_with_executor(Executor::default()).boxed()
}

#[cfg(target_os = "macos")]
fn build_unix_client(path: &str) -> RamaHttpClient {
    EasyHttpWebClient::connector_builder()
        .with_custom_transport_connector(UnixConnector::fixed(path))
        .without_tls_proxy_support()
        .without_proxy_support()
        .without_tls_support()
        .with_default_http_connector::<Body>(Executor::default())
        .build_client()
        .boxed()
}
