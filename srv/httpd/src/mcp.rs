//! The host owns routing, POST ingress capacity and middleware; mcp-host owns
//! the MCP transport. The shared HTTP permit ends when SDK response creation
//! finishes, not when a lazy SSE body or task ends. SDK per-session SSE bounds
//! and mcpd's four-root-workflow session admission independently limit that work.

use chaos_snitch::tracing;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use chaos_mcpd::McpServerHandle;
use mcp_host::transport::http::{
    HttpBearerAuthConfig, HttpTransportConfig, HttpTransportSessionHandle, McpHttpService,
};
use rama::http::headers::{AccessControlExposeHeaders, AccessControlMaxAge};
use rama::http::layer::catch_panic::CatchPanicLayer;
use rama::http::layer::cors::CorsLayer;
use rama::http::layer::timeout::TimeoutLayer;
use rama::http::service::web::response::IntoResponse;
use rama::http::{Body, HeaderName, Request, Response, StatusCode};
use rama::utils::collections::non_empty_vec;
use rama::{Layer, Service};
use tokio::sync::Mutex;

use crate::ServeCli;

pub(crate) fn validate_cli(cli: &ServeCli) -> anyhow::Result<()> {
    if cli.mcp && cli.session_recovery_only {
        bail!("--mcp cannot be used with --session-recovery-only");
    }
    if !cli.mcp && !cli.mcp_allowed_origin.is_empty() {
        bail!("--mcp-allowed-origin requires --mcp");
    }
    if cli.mcp
        && let Some(token) = &cli.bearer_token
    {
        HttpBearerAuthConfig::new(token.clone(), "chaos-operator")
            .context("invalid MCP operator bearer credential")?;
    }
    for origin in &cli.mcp_allowed_origin {
        let url = url::Url::parse(origin).context("invalid --mcp-allowed-origin")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.origin().ascii_serialization() != *origin
        {
            bail!("--mcp-allowed-origin must be an exact HTTP(S) origin without a path");
        }
    }
    Ok(())
}

fn transport_config(cli: &ServeCli, token: &str) -> anyhow::Result<HttpTransportConfig> {
    let credential = HttpBearerAuthConfig::new(token, "chaos-operator")
        .context("invalid MCP operator bearer credential")?;
    Ok(HttpTransportConfig {
        endpoint: "/mcp".to_owned(),
        localhost_only: cli
            .bind
            .parse::<std::net::IpAddr>()
            .context("invalid bind address")?
            .is_loopback(),
        allowed_origins: cli.mcp_allowed_origin.clone(),
        bearer_auth: Some(credential.into()),
        max_request_body_size: cli.body_limit,
        request_timeout: Duration::from_secs(cli.timeout),
        session_timeout: Duration::from_secs(cli.session_ttl),
        max_sessions: cli.max_sessions,
        max_sse_connections: cli.max_concurrent,
        // Keep the SDK's bounded replay, outbound and held-message queues.
        ..HttpTransportConfig::default()
    })
}

pub(crate) struct McpEndpoint {
    service: Mutex<Option<McpHttpService>>,
    sessions: HttpTransportSessionHandle,
    timeout: Duration,
    kernel: Mutex<Option<McpServerHandle>>,
}

impl McpEndpoint {
    pub(crate) fn new(
        kernel: McpServerHandle,
        cli: &ServeCli,
        token: &str,
    ) -> anyhow::Result<Self> {
        let config = transport_config(cli, token)?;
        let service = McpHttpService::new(Arc::clone(&kernel.server), config.clone())?;
        let sessions = service.session_handle();
        Ok(Self {
            service: Mutex::new(Some(service)),
            sessions,
            timeout: config.request_timeout,
            kernel: Mutex::new(Some(kernel)),
        })
    }

    pub(crate) async fn handle(&self, request: Request) -> Response {
        let Some(service) = self.service.lock().await.clone() else {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Body::empty())
                .expect("static shutdown response");
        };
        // Mirror the SDK listener's middleware. OPTIONS must reach the SDK to
        // validate Origin; CORS must not synthesize a successful preflight.
        let layers = (
            cors_layer(),
            TimeoutLayer::new(self.timeout),
            CatchPanicLayer::new(),
        );
        match layers.into_layer(service).serve(request).await {
            Ok(response) => response.into_response(),
            Err(never) => match never {},
        }
    }

    pub(crate) async fn shutdown(&self) {
        // Revoke admission before cleanup: otherwise a new initialize/GET can
        // race session termination and keep graceful HTTP drainage alive.
        let service = self.service.lock().await.take();
        for session in self.sessions.sessions() {
            self.sessions.terminate_session(&session.id);
        }
        drop(service);
        if let Some(mut kernel) = self.kernel.lock().await.take()
            && let Err(error) = kernel.shutdown().await
        {
            tracing::warn!(%error, "MCP ownership cleanup failed");
        }
    }
}

fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .with_allow_origin_mirror_request()
        .with_allow_methods_mirror_request()
        .with_allow_headers_mirror_request()
        .try_with_allow_credentials()
        .expect("mirrored CORS headers support credentials")
        .try_with_expose_headers(AccessControlExposeHeaders::new_values(non_empty_vec![
            HeaderName::from_static("mcp-session-id"),
            HeaderName::from_static("mcp-protocol-version"),
        ]))
        .expect("explicit exposed headers support credentials")
        .with_max_age(AccessControlMaxAge::from_seconds(86400))
        .handle_options_request()
}

#[cfg(test)]
mod tests;
