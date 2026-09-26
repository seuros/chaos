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
        Request, Response, StatusCode,
        layer::upgrade::{LazyHttpProxyConnectReplyService, UpgradeLayer, Upgraded},
        matcher::MethodMatcher,
        server::HttpServer,
    },
    layer::{AddInputExtensionLayer, ConsumeErrLayer},
    net::address::Domain,
    rt::Executor,
    service::service_fn,
    tcp::server::TcpListener,
    tls::rustls::server::TlsAcceptorLayer,
    tls::server::{
        CertificateIdentity, CertificateSubject, LeafCertConfig, LeafCertRequest,
        SelfSignedCaConfig, ServerAuthData, TlsServerConfig,
    },
};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::proxy::{
    RecordParts, WiretapExchange, WiretapSink, error_response, forward_recorded_request,
};

mod prompt;
use prompt::AntigravitySystemPrompt;

/// Hosts `agy` is known to need: the Cloud Code agent backend, the OAuth token
/// endpoint, and the generative-language surface. Everything else the binary
/// references (telemetry, Play, mTLS variants) is deliberately absent.
pub const ANTIGRAVITY_ALLOWED_HOSTS: [&str; 4] = [
    "cloudcode-pa.googleapis.com",
    "daily-cloudcode-pa.googleapis.com",
    "oauth2.googleapis.com",
    "generativelanguage.googleapis.com",
];

/// Which destinations a clamped subprocess may reach, and whether the proxy
/// opens the TLS session to read what crosses it.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    allowed_hosts: Vec<String>,
    inspect_bodies: bool,
    gateway: Option<chaos_client::Egress>,
    system_prompt: Option<AntigravitySystemPrompt>,
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
            gateway: None,
            system_prompt: None,
        }
    }

    /// The default policy for Google's Antigravity CLI.
    pub fn antigravity() -> Self {
        Self::new(ANTIGRAVITY_ALLOWED_HOSTS)
    }

    /// Replace AGY's system instructions at the generation-request boundary.
    /// Empty text removes CLI instructions rather than keeping its defaults.
    pub fn with_antigravity_system_prompt(mut self, text: String) -> Self {
        self.system_prompt = Some(AntigravitySystemPrompt::new(text));
        self
    }

    /// Route permitted requests through the global LSD gateway after TLS
    /// termination. The destination allowlist still applies before forwarding.
    pub fn with_gateway(mut self, gateway: Option<chaos_client::Egress>) -> Self {
        self.gateway = gateway;
        self
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
    system_prompt: Option<AntigravitySystemPrompt>,
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
        if policy.gateway.is_some() && !policy.inspect_bodies {
            return Err(
                "global egress requires TLS inspection; direct tunnel relay is disabled".into(),
            );
        }
        if policy.system_prompt.is_some() && !policy.inspect_bodies {
            return Err("clamp system-prompt replacement requires TLS inspection".into());
        }
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

        let system_prompt = policy.system_prompt.clone();
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
            system_prompt,
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

    /// Publish the canonical Chaos prompt before starting a fresh or resumed
    /// AGY turn. All generation requests in its tool loop use this snapshot.
    pub fn set_antigravity_system_prompt(&self, text: String) -> Result<(), BoxError> {
        self.system_prompt
            .as_ref()
            .ok_or("clamp egress was started without system-prompt replacement")?
            .update(text)
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

    let req = if let Some(prompt) = &state.policy.system_prompt {
        match prompt.rewrite(req, &host).await {
            Ok(req) => req,
            Err(err) => {
                warn!("egress: system-prompt replacement failed: {err}");
                return Ok(error_response(StatusCode::BAD_GATEWAY));
            }
        }
    } else {
        req
    };
    // Record the effective provider request, not the discarded CLI prompt.
    let (mut upstream_req, record) = match RecordParts::capture(req).await {
        Ok(captured) => captured,
        Err(err) => {
            warn!("egress: failed to read request body: {err}");
            return Ok(error_response(StatusCode::BAD_GATEWAY));
        }
    };
    if state.policy.gateway.is_some() {
        // HTTP/1 requests inside CONNECT can use origin-form targets.
        if upstream_req.uri().scheme().is_none() {
            upstream_req
                .uri_mut()
                .set_scheme(rama::net::Protocol::HTTPS);
        }
        if upstream_req.uri().authority().is_none() {
            let Ok(authority) = host.parse() else {
                return Ok(error_response(StatusCode::BAD_GATEWAY));
            };
            upstream_req.uri_mut().set_authority(authority);
        }
    }

    forward_recorded_request(
        upstream_req,
        record,
        state.sink,
        state.exec,
        state.policy.gateway.as_ref(),
    )
    .await
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

#[cfg(test)]
mod tests;
