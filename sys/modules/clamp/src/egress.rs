//! Egress proxy: a loopback HTTP `CONNECT` proxy that is the only route out of
//! a sandboxed first-party CLI.
//!
//! Unlike [`crate::proxy`], which the clamped process opts into by way of a base
//! URL it was handed, this proxy is paired with a kernel sandbox that permits
//! exactly one TCP destination port — the one this listener bound. A subprocess
//! that ignores `HTTPS_PROXY` and dials an upstream directly does not reach a
//! policy check; it fails to connect at all.
//!
//! The proxy therefore owns what the kernel cannot express. Landlock network
//! rules are scoped to a port, not a host, so the destination allowlist lives
//! here: a `CONNECT` to a host outside the policy is answered with `403` and
//! recorded, and the tunnel is never opened.
//!
//! With `inspect_bodies` the proxy terminates TLS using a session certificate
//! authority and re-originates the connection upstream, which puts request and
//! response bodies through the same [`WiretapSink`] the Claude Code wiretap
//! uses. The subprocess must be pointed at [`EgressProxy::ca_bundle_path`] for
//! this to verify — Go, Python, and Node all honor `SSL_CERT_FILE` or an
//! equivalent. Without it the proxy relays the tunnel verbatim, which still
//! enforces the allowlist and records connection attempts but sees no bodies.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rama::{
    Layer, Service,
    error::BoxError,
    extensions::{Extension, ExtensionsRef},
    http::{
        Body, Request, Response, StatusCode, Version,
        body::util::BodyExt,
        client::EasyHttpWebClient,
        layer::{
            map_response_body::MapResponseBodyLayer,
            remove_header::{RemoveRequestHeaderLayer, RemoveResponseHeaderLayer},
            upgrade::{LazyHttpProxyConnectReplyService, UpgradeLayer, Upgraded},
        },
        matcher::MethodMatcher,
        server::HttpServer,
    },
    layer::{AddInputExtensionLayer, ConsumeErrLayer},
    net::address::Domain,
    rt::Executor,
    service::service_fn,
    tcp::server::TcpListener,
    tls::client::TlsClientConfig,
    tls::rustls::server::TlsAcceptorLayer,
    tls::server::{
        CertificateIdentity, CertificateSubject, LeafCertConfig, LeafCertRequest,
        SelfSignedCaConfig, ServerAuthData, TlsServerConfig,
    },
};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::proxy::{RecordParts, TeeBody, WiretapExchange, WiretapSink, redact_headers};

/// Hosts `agy` is known to need: the Cloud Code agent backend, the OAuth token
/// endpoint, and the generative-language surface. Everything else the binary
/// references (telemetry, Play, mTLS variants) is deliberately absent.
pub const ANTIGRAVITY_ALLOWED_HOSTS: [&str; 3] = [
    "cloudcode-pa.googleapis.com",
    "oauth2.googleapis.com",
    "generativelanguage.googleapis.com",
];

/// Cap on buffered response bytes per exchange before the record is truncated.
const MAX_RESPONSE_CAPTURE: usize = 8 * 1024 * 1024;

/// Which destinations a clamped subprocess may reach, and whether the proxy
/// opens the TLS session to read what crosses it.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    allowed_hosts: Vec<String>,
    inspect_bodies: bool,
}

impl EgressPolicy {
    /// Build a policy from an exact-match host allowlist. Hosts are compared
    /// case-insensitively; no wildcards, because a wildcard on a shared apex
    /// like `googleapis.com` would re-admit every endpoint the allowlist exists
    /// to exclude.
    pub fn new<I, S>(allowed_hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(|host| host.as_ref().trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect(),
            inspect_bodies: true,
        }
    }

    /// The default policy for Google's Antigravity CLI.
    pub fn antigravity() -> Self {
        Self::new(ANTIGRAVITY_ALLOWED_HOSTS)
    }

    /// Relay tunnels verbatim instead of terminating TLS. Use when the
    /// subprocess pins certificates: the allowlist still holds, but bodies are
    /// opaque and only the `CONNECT` target is recorded.
    pub fn without_body_inspection(mut self) -> Self {
        self.inspect_bodies = false;
        self
    }

    /// Whether `host` (no port) is permitted.
    pub fn permits(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        self.allowed_hosts.contains(&host)
    }

    fn allowed_hosts(&self) -> &[String] {
        &self.allowed_hosts
    }
}

/// A running egress proxy. Dropping it (or calling [`EgressProxy::shutdown`])
/// stops the listener, which is what makes the sandbox's single permitted port
/// dead the moment the turn is over.
pub struct EgressProxy {
    port: u16,
    ca_bundle_path: Option<PathBuf>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for EgressProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressProxy")
            .field("port", &self.port)
            .field("ca_bundle_path", &self.ca_bundle_path)
            .finish()
    }
}

/// Per-connection state handed to the upgrade handler.
#[derive(Clone, Extension)]
struct EgressState {
    policy: EgressPolicy,
    sink: Arc<dyn WiretapSink>,
    tls: Option<TlsServerConfig>,
    exec: Executor,
}

impl std::fmt::Debug for EgressState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressState")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl EgressProxy {
    /// Start an egress proxy on an OS-assigned loopback port.
    ///
    /// When the policy inspects bodies, `ca_bundle_path` receives the session
    /// certificate authority in PEM form (owner-readable only) and must be
    /// handed to the subprocess as its trust root.
    ///
    /// # Errors
    /// Returns an error if the listener cannot bind, or if the session
    /// certificate cannot be generated or written.
    pub async fn start(
        policy: EgressPolicy,
        sink: Arc<dyn WiretapSink>,
        ca_bundle_path: Option<PathBuf>,
    ) -> Result<Self, BoxError> {
        let (tls, ca_bundle_path) = if policy.inspect_bodies {
            let path = ca_bundle_path
                .ok_or_else(|| BoxError::from("body inspection requires a CA bundle path"))?;
            let auth = session_authority(policy.allowed_hosts())?;
            write_ca_bundle(&path, &auth)?;
            let tls = TlsServerConfig::new()
                .with_server_auth(auth)
                .with_alpn_http_auto();
            (Some(tls), Some(path))
        } else {
            (None, None)
        };

        let exec = Executor::default();
        let listener = TcpListener::build(exec.clone())
            .bind_address("127.0.0.1:0")
            .await?;
        let port = listener.local_addr()?.port();

        let state = EgressState {
            policy,
            sink,
            tls,
            exec: exec.clone(),
        };

        let http = HttpServer::auto(exec).service(Arc::new(
            (
                ConsumeErrLayer::default(),
                UpgradeLayer::new_with_services(
                    Executor::default(),
                    MethodMatcher::CONNECT,
                    AllowlistConnectReply,
                    service_fn(handle_tunnel),
                ),
            )
                .into_layer(Arc::new(service_fn(reject_plaintext))),
        ));

        let task = tokio::spawn(async move {
            listener
                .serve(AddInputExtensionLayer::new(state).into_layer(http))
                .await;
        });

        info!(port, "clamp egress proxy listening on loopback");
        Ok(Self {
            port,
            ca_bundle_path,
            task,
        })
    }

    /// The loopback port the proxy is listening on. This is the single port the
    /// sandbox permits the subprocess to connect to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The value to export as `HTTPS_PROXY`/`HTTP_PROXY`.
    pub fn proxy_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Path to the session CA bundle, when body inspection is enabled.
    pub fn ca_bundle_path(&self) -> Option<&Path> {
        self.ca_bundle_path.as_deref()
    }

    /// Stop the proxy.
    pub fn shutdown(self) {
        self.task.abort();
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Answers `CONNECT` only for allowlisted hosts; everything else is refused
/// before a tunnel exists.
#[derive(Debug, Clone)]
struct AllowlistConnectReply;

impl<Body_> Service<Request<Body_>> for AllowlistConnectReply
where
    Body_: Send + 'static,
{
    type Output = <LazyHttpProxyConnectReplyService as Service<Request<Body_>>>::Output;
    type Error = Response;

    async fn serve(&self, req: Request<Body_>) -> Result<Self::Output, Self::Error> {
        let target = req.uri().to_string();
        let host = connect_host(&target);

        let Some(state) = req.extensions().get_ref::<EgressState>().cloned() else {
            warn!("egress: proxy state missing from connection extensions");
            return Err(error_response(StatusCode::INTERNAL_SERVER_ERROR));
        };

        match host {
            Some(host) if state.policy.permits(&host) => {
                debug!(host = %host, "egress: CONNECT permitted");
                LazyHttpProxyConnectReplyService::new().serve(req).await
            }
            other => {
                let host = other.unwrap_or_else(|| target.clone());
                warn!(host = %host, "egress: CONNECT blocked by allowlist");
                state.sink.record(blocked_exchange(&host));
                Err(error_response(StatusCode::FORBIDDEN))
            }
        }
    }
}

/// A subprocess that speaks plaintext HTTP through the proxy is refused: every
/// allowlisted destination is HTTPS, so a plain request is either a mistake or
/// an attempt to downgrade.
async fn reject_plaintext(req: Request) -> Result<Response, std::convert::Infallible> {
    let target = req.uri().to_string();
    warn!(target = %target, "egress: non-CONNECT request refused");
    if let Some(state) = req.extensions().get_ref::<EgressState>() {
        state.sink.record(blocked_exchange(&target));
    }
    Ok(error_response(StatusCode::FORBIDDEN))
}

/// Handles an accepted tunnel: terminate TLS and record, or relay bytes.
async fn handle_tunnel(upgraded: Upgraded) -> Result<(), BoxError> {
    let state = upgraded
        .extensions()
        .get_ref::<EgressState>()
        .cloned()
        .ok_or_else(|| BoxError::from("egress: proxy state missing from upgraded connection"))?;

    let Some(tls) = state.tls.clone() else {
        // Relay mode: rama has already resolved the CONNECT target, so the
        // tunnel is a straight byte pump with no visibility into the session.
        return relay_tunnel(upgraded).await;
    };

    let inner = Arc::new(service_fn(move |req: Request| {
        let state = state.clone();
        async move { inspect_and_forward(req, state).await }
    }));
    let http = HttpServer::auto(state_exec(&upgraded)).service(inner);
    TlsAcceptorLayer::new(tls)
        .with_store_client_hello(true)
        .into_layer(http)
        .serve(upgraded)
        .await?;
    Ok(())
}

fn state_exec(upgraded: &Upgraded) -> Executor {
    upgraded
        .extensions()
        .get_ref::<EgressState>()
        .map(|state| state.exec.clone())
        .unwrap_or_default()
}

/// Byte-for-byte tunnel used when TLS is not terminated.
async fn relay_tunnel(upgraded: Upgraded) -> Result<(), BoxError> {
    let target = upgraded
        .extensions()
        .get_ref::<rama::net::client::ConnectorTarget>()
        .cloned()
        .ok_or_else(|| BoxError::from("egress: tunnel has no connector target"))?;

    let mut server = tokio::net::TcpStream::connect(target.0.to_string()).await?;
    let mut client = upgraded;
    tokio::io::copy_bidirectional(&mut client, &mut server).await?;
    Ok(())
}

/// Serves one request from inside the terminated TLS session: re-checks the
/// destination, forwards it upstream, and tees both directions into the sink.
async fn inspect_and_forward(
    req: Request,
    state: EgressState,
) -> Result<Response, std::convert::Infallible> {
    let host = req
        .uri()
        .host()
        .map(|host| host.to_string())
        .or_else(|| {
            req.headers()
                .get(rama::http::header::HOST)
                .and_then(|value| value.to_str().ok())
                .map(|value| connect_host(value).unwrap_or_else(|| value.to_string()))
        })
        .unwrap_or_default();

    // The CONNECT target was checked before the tunnel opened; a request whose
    // own authority disagrees is checked again rather than trusted.
    if !state.policy.permits(&host) {
        warn!(host = %host, "egress: in-tunnel request blocked by allowlist");
        state.sink.record(blocked_exchange(&host));
        return Ok(error_response(StatusCode::FORBIDDEN));
    }

    let (mut parts, body) = req.into_parts();
    let method = parts.method.to_string();
    let path = parts.uri.request_target().into_owned();
    let headers = redact_headers(&parts.headers);

    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => {
            warn!("egress: failed to read request body: {err}");
            return Ok(error_response(StatusCode::BAD_GATEWAY));
        }
    };
    let request = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok());
    let record = RecordParts::new(method, path, headers, request);

    // Ask for identity encoding so the tee captures readable bytes.
    parts.headers.remove("accept-encoding");
    let upstream_req = Request::from_parts(parts, Body::from(bytes));

    let tls = TlsClientConfig::new().with_alpn_http_auto();
    let client = EasyHttpWebClient::connector_builder()
        .with_default_transport_connector()
        .with_default_dns_connector()
        .with_tls_proxy_support_using_rustls()
        .with_proxy_support()
        .with_tls_support_using_rustls_and_default_http_version(tls, Version::HTTP_11)
        .with_default_http_connector(state.exec.clone())
        .without_connection_pool()
        .build_client();
    let client = (
        RemoveRequestHeaderLayer::hop_by_hop(),
        RemoveResponseHeaderLayer::hop_by_hop(),
        MapResponseBodyLayer::new_boxed_streaming_body(),
    )
        .into_layer(client);

    match client.serve(upstream_req).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let (resp_parts, resp_body) = resp.into_parts();
            let tee = TeeBody::new(
                resp_body.into_data_stream(),
                record,
                status,
                state.sink,
                MAX_RESPONSE_CAPTURE,
            );
            Ok(Response::from_parts(resp_parts, Body::from_stream(tee)))
        }
        Err(err) => {
            warn!("egress: upstream error: {err:?}");
            state.sink.record(record.into_exchange(None, None, false));
            Ok(error_response(StatusCode::BAD_GATEWAY))
        }
    }
}

/// Generates the session CA and a leaf covering every allowlisted host.
///
/// One leaf with every allowlisted name as a SAN is enough because the
/// allowlist is small and fixed for the life of the proxy; there is no need to
/// mint a certificate per connection.
fn session_authority(allowed_hosts: &[String]) -> Result<ServerAuthData, BoxError> {
    let mut sans = Vec::with_capacity(allowed_hosts.len());
    for host in allowed_hosts {
        sans.push(
            Domain::try_from(host.clone())
                .map_err(|err| BoxError::from(format!("invalid allowlist host {host}: {err}")))?,
        );
    }
    let organisation_name = Some("Chaos Clamp Egress".to_owned());
    let leaf = LeafCertRequest {
        config: LeafCertConfig {
            subject: CertificateSubject {
                organisation_name: organisation_name.clone(),
                common_name: sans.first().map(ToString::to_string),
            },
            ..Default::default()
        },
        identities: sans.into_iter().map(CertificateIdentity::from).collect(),
    };
    ServerAuthData::new_generated_ca(
        SelfSignedCaConfig {
            subject: CertificateSubject {
                organisation_name,
                common_name: Some("Chaos Clamp Egress CA".to_owned()),
            },
            ..Default::default()
        },
        leaf,
    )
}

/// Writes the CA (the last entry of the generated chain) as a PEM bundle with
/// owner-only permissions, so the trust root Chaos mints cannot be read — or
/// swapped — by another user on the machine.
fn write_ca_bundle(path: &Path, auth: &ServerAuthData) -> Result<(), BoxError> {
    let ca = auth
        .cert_chain
        .last()
        .ok_or_else(|| BoxError::from("session certificate chain is empty"))?;
    let pem = pem_encode("CERTIFICATE", ca.as_ref());
    crate::antigravity::atomic_write_private(path, pem.as_bytes())?;
    Ok(())
}

fn pem_encode(label: &str, der: &[u8]) -> String {
    use base64::Engine;
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// Extracts the host from a `CONNECT` target (`host:port`), tolerating bracketed
/// IPv6 literals and a scheme-prefixed absolute form.
fn connect_host(target: &str) -> Option<String> {
    let target = target.trim();
    let target = target
        .split_once("://")
        .map_or(target, |(_, rest)| rest)
        .split(['/', '?'])
        .next()?;
    if let Some(rest) = target.strip_prefix('[') {
        return rest
            .split_once(']')
            .map(|(host, _)| host.to_ascii_lowercase());
    }
    let host = target.rsplit_once(':').map_or(target, |(host, _)| host);
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn blocked_exchange(host: &str) -> WiretapExchange {
    WiretapExchange {
        method: "CONNECT".to_string(),
        path: host.to_string(),
        headers: serde_json::json!({}),
        request: None,
        status: Some(StatusCode::FORBIDDEN.as_u16()),
        response: Some("blocked by egress allowlist".to_string()),
        response_truncated: false,
    }
}

fn error_response(status: StatusCode) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = status;
    resp
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestSink {
        recorded: Mutex<Vec<WiretapExchange>>,
    }

    impl WiretapSink for TestSink {
        fn record(&self, exchange: WiretapExchange) {
            self.recorded.lock().unwrap().push(exchange);
        }
    }

    #[test]
    fn allowlist_matches_exact_hosts_and_parses_connect_targets() {
        let policy = EgressPolicy::antigravity();

        assert!(policy.permits("cloudcode-pa.googleapis.com"));
        // Case and a trailing root dot are normalized away.
        assert!(policy.permits("CloudCode-PA.googleapis.com."));
        // Sibling endpoints on the same apex are not implied by an allowed one.
        assert!(!policy.permits("aiplatform.googleapis.com"));
        assert!(!policy.permits("play.googleapis.com"));
        // No suffix confusion: an attacker-controlled parent is not a match.
        assert!(!policy.permits("cloudcode-pa.googleapis.com.evil.test"));
        assert!(!policy.permits("evil.test"));

        assert_eq!(
            connect_host("cloudcode-pa.googleapis.com:443").as_deref(),
            Some("cloudcode-pa.googleapis.com")
        );
        assert_eq!(
            connect_host("https://oauth2.googleapis.com/token").as_deref(),
            Some("oauth2.googleapis.com")
        );
        assert_eq!(connect_host("[::1]:8443").as_deref(), Some("::1"));
        assert_eq!(connect_host(""), None);
    }

    #[tokio::test]
    async fn session_ca_is_written_as_owner_only_pem_covering_the_allowlist() {
        let dir = tempfile::tempdir().expect("temporary state directory");
        let path = dir.path().join("nested").join("egress-ca.pem");
        let sink = Arc::new(TestSink::default());

        let proxy = EgressProxy::start(EgressPolicy::antigravity(), sink, Some(path.clone()))
            .await
            .expect("start egress proxy");

        assert_eq!(proxy.ca_bundle_path(), Some(path.as_path()));
        assert_eq!(
            proxy.proxy_url(),
            format!("http://127.0.0.1:{}", proxy.port())
        );

        let pem = std::fs::read_to_string(&path).expect("read ca bundle");
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.trim_end().ends_with("-----END CERTIFICATE-----"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "ca bundle must not be world readable");
        }

        proxy.shutdown();
    }

    #[tokio::test]
    async fn blocked_destinations_are_refused_and_recorded() {
        let dir = tempfile::tempdir().expect("temporary state directory");
        let sink = Arc::new(TestSink::default());
        let proxy = EgressProxy::start(
            EgressPolicy::antigravity(),
            sink.clone(),
            Some(dir.path().join("ca.pem")),
        )
        .await
        .expect("start egress proxy");

        // Raw CONNECT to a host outside the allowlist: refused before a tunnel
        // exists, so the subprocess never gets bytes to or from the upstream.
        let status = connect_status(proxy.port(), "aiplatform.googleapis.com:443").await;
        assert_eq!(status, 403);

        // A plaintext (non-CONNECT) request is refused for the same reason.
        let status = connect_status(proxy.port(), "http://evil.test/exfil").await;
        assert_eq!(status, 403);

        let recorded = sink.recorded.lock().unwrap();
        assert_eq!(recorded.len(), 2, "both attempts recorded");
        assert!(recorded.iter().any(|e| e.path.contains("aiplatform")));
        assert!(recorded.iter().any(|e| e.path.contains("evil.test")));
        drop(recorded);

        proxy.shutdown();
    }

    /// Sends a bare request line to the proxy and returns the status code.
    /// An absolute-form target is sent as `GET`, a `host:port` as `CONNECT`.
    async fn connect_status(port: u16, target: &str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let method = if target.contains("://") {
            "GET"
        } else {
            "CONNECT"
        };
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect to proxy");
        let request = format!("{method} {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 128];
        let read = stream.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..read]);
        head.split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn session_authority_mints_a_ca_backed_leaf_covering_every_allowlisted_host() {
        let hosts: Vec<String> = ANTIGRAVITY_ALLOWED_HOSTS
            .iter()
            .map(|host| (*host).to_owned())
            .collect();
        let auth = session_authority(&hosts).expect("mint session authority");

        // The chain is leaf-first with the session CA last: `write_ca_bundle`
        // publishes that last entry as the trust root the child process pins.
        assert_eq!(auth.cert_chain.len(), 2);
        let leaf = auth.cert_chain.first().unwrap().as_ref();
        let ca = auth.cert_chain.last().unwrap().as_ref();
        assert_ne!(leaf, ca);

        // Every allowlisted name is a SAN on the single leaf, and none of them
        // leak into the CA.
        for host in &hosts {
            assert!(
                leaf.windows(host.len()).any(|w| w == host.as_bytes()),
                "leaf is missing SAN {host}"
            );
            assert!(
                !ca.windows(host.len()).any(|w| w == host.as_bytes()),
                "CA unexpectedly carries {host}"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ca.pem");
        write_ca_bundle(&path, &auth).expect("write ca bundle");
        let pem = std::fs::read_to_string(&path).unwrap();
        assert_eq!(pem, pem_encode("CERTIFICATE", ca));

        session_authority(&["not a domain".to_owned()])
            .expect_err("non-domain allowlist entry must be rejected");
    }
}
