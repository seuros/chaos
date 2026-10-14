//! Server-owned, at-least-once delivery. No redirect, body collection or executor replay.
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chaos_journald::{
    ExecutionDelivery, ExecutionDeliveryRequest as DeliveryRequest, ExecutionRequest,
    ExecutionResponse,
};
use hmac::{Hmac, KeyInit, Mac};
use rama::{
    Service,
    http::{Body, Request},
};
use sha2::Sha256;

use crate::sessions::{SessionManager, durable::Store};

pub(crate) struct WebhookConfig {
    url: String,
    secret: String,
    digest: String,
}

impl WebhookConfig {
    pub(crate) fn from_env(
        url: &str,
        env: &str,
        bearer: &str,
        recovery: Option<&str>,
    ) -> anyhow::Result<Self> {
        let secret = std::env::var(env)
            .map_err(|_| anyhow::anyhow!("webhook signing key is unavailable"))?;
        anyhow::ensure!(
            secret != bearer && recovery != Some(secret.as_str()),
            "webhook signing key must differ from bearer and recovery credentials"
        );
        Self::new(url, secret)
    }

    pub(crate) fn new(url: &str, secret: String) -> anyhow::Result<Self> {
        let parsed = url::Url::parse(url).map_err(|_| anyhow::anyhow!("invalid webhook URL"))?;
        let loopback = parsed
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback());
        anyhow::ensure!(
            url.len() <= 2048
                && parsed.host().is_some()
                && (parsed.scheme() == "https" || parsed.scheme() == "http" && loopback)
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.fragment().is_none(),
            "webhook URL must use HTTPS (or literal loopback HTTP), without userinfo or fragment",
        );
        anyhow::ensure!(
            secret.len() >= 32 && !secret.trim().is_empty(),
            "webhook signing key must contain at least 32 bytes"
        );
        let url = parsed.to_string();
        let fingerprint = chaos_kern::user_settings::fingerprint(&serde_json::json!({
            "format": "httpd-webhook/v1", "url": url, "key": secret,
        }))?;
        Ok(Self {
            url,
            secret,
            digest: fingerprint
                .strip_prefix("v1:")
                .expect("fingerprint version")
                .into(),
        })
    }

    pub(crate) async fn configure(&self, manager: &SessionManager) -> anyhow::Result<()> {
        let store = manager
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("webhooks require durable sessions"))?;
        store
            .call(ExecutionRequest::Delivery {
                request: DeliveryRequest::Configure {
                    scope: store.scope.clone(),
                    destination_digest: self.digest.clone(),
                },
            })
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "webhook destination/signing identity conflicts or storage is unavailable"
                )
            })?;
        Ok(())
    }

    pub(crate) fn check_environment(
        &self,
        config: &chaos_kern::config::Config,
        env: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !chaos_kern::exec_env::create_env(&config.permissions.shell_environment_policy, None)
                .contains_key(env),
            "webhook signing variable must be excluded from the command environment",
        );
        for server in config.mcp_servers.get().values() {
            let material = serde_json::to_value(server)?;
            let selected = material
                .get("env_vars")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|vars| vars.iter().any(|v| v.as_str() == Some(env)))
                || material
                    .get("env")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|vars| vars.contains_key(env))
                || material
                    .get("bearer_token_env_var")
                    .and_then(serde_json::Value::as_str)
                    == Some(env)
                || material
                    .get("env_http_headers")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|headers| headers.values().any(|v| v.as_str() == Some(env)));
            anyhow::ensure!(
                !selected,
                "webhook signing variable must not be selected by MCP tools"
            );
        }
        Ok(())
    }

    fn request(&self, delivery: &ExecutionDelivery, timestamp: u64) -> Request {
        let timestamp = timestamp.to_string();
        let event_id = delivery.status.event_id.to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
            .expect("HMAC accepts any key size");
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(event_id.as_bytes());
        mac.update(b".");
        mac.update(delivery.body_json.as_bytes());
        let signature: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Request::builder()
            .method("POST")
            .uri(self.url.as_str())
            .header("content-type", "application/json")
            .header("x-chaos-event-id", event_id)
            .header("x-chaos-timestamp", timestamp)
            .header("x-chaos-signature", format!("v1={signature}"))
            .header(
                "x-chaos-delivery-attempt",
                delivery.status.attempts.to_string(),
            )
            .body(Body::from(delivery.body_json.clone()))
            .expect("validated webhook headers and URL")
    }

    async fn claim(
        &self,
        store: &Store,
        owner_id: &str,
    ) -> anyhow::Result<Option<ExecutionDelivery>> {
        match store
            .call(ExecutionRequest::Delivery {
                request: DeliveryRequest::Claim {
                    scope: store.scope.clone(),
                    destination_digest: self.digest.clone(),
                    owner_id: owner_id.into(),
                    ttl_ms: 60_000,
                },
            })
            .await
        {
            Ok(ExecutionResponse::Delivery(delivery)) => Ok(delivery),
            _ => Err(anyhow::anyhow!("webhook claim unavailable")),
        }
    }

    async fn settle(
        &self,
        store: &Store,
        delivery: ExecutionDelivery,
        success: bool,
    ) -> anyhow::Result<()> {
        let scope = store.scope.clone();
        let destination_digest = self.digest.clone();
        let lease = delivery.lease;
        let request = if success {
            DeliveryRequest::Acknowledge {
                scope,
                destination_digest,
                lease,
            }
        } else {
            DeliveryRequest::Retry {
                scope,
                destination_digest,
                lease,
            }
        };
        match store.call(ExecutionRequest::Delivery { request }).await {
            Ok(ExecutionResponse::DeliveryRecorded) => Ok(()),
            _ => Err(anyhow::anyhow!("webhook acknowledgement unavailable")),
        }
    }

    pub(crate) fn spawn(self, manager: Arc<SessionManager>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Deliberately no model credentials, infrastructure cookies or redirect middleware.
            let _ =
                rama::tls::rustls::dep::rustls::crypto::ring::default_provider().install_default();
            let client = rama::http::client::EasyHttpWebClient::default();
            let mut shutdown = manager.shutdown.subscribe();
            let store = manager.store.as_ref().expect("configured durable store");
            while !*shutdown.borrow() {
                let attempt = async {
                    let Some(delivery) = self.claim(store, &manager.daemon_id).await? else {
                        return Ok::<_, anyhow::Error>(false);
                    };
                    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                    let request = self.request(&delivery, timestamp);
                    let success = matches!(
                        tokio::time::timeout(Duration::from_secs(10), client.serve(request)).await,
                        Ok(Ok(response)) if response.status().is_success()
                    );
                    // The receiver may already have committed even on timeout or lost ack.
                    self.settle(store, delivery, success).await?;
                    Ok(true)
                };
                let result = tokio::select! {
                    _ = shutdown.changed() => break,
                    result = attempt => result,
                };
                let delay = match result {
                    Ok(true) => Duration::from_millis(100),
                    Ok(false) => Duration::from_secs(1),
                    Err(_) => {
                        // No URL, key, response body or private event material in logs.
                        tracing::warn!(
                            "webhook storage/delivery acknowledgement unavailable; retry retained"
                        );
                        Duration::from_secs(1)
                    }
                };
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        })
    }
}
