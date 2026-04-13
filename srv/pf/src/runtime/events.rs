use crate::config::NetworkMode;
use jiff::Timestamp;
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::debug;

pub(super) const MAX_BLOCKED_EVENTS: usize = 200;
pub(super) const NETWORK_POLICY_VIOLATION_PREFIX: &str = "CHAOS_NETWORK_POLICY_VIOLATION";

#[derive(Clone, Debug, Serialize)]
pub struct BlockedRequest {
    pub host: String,
    pub reason: String,
    pub client: Option<String>,
    pub method: Option<String>,
    pub mode: Option<NetworkMode>,
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub timestamp: i64,
}

pub struct BlockedRequestArgs {
    pub host: String,
    pub reason: String,
    pub client: Option<String>,
    pub method: Option<String>,
    pub mode: Option<NetworkMode>,
    pub protocol: String,
    pub decision: Option<String>,
    pub source: Option<String>,
    pub port: Option<u16>,
}

impl BlockedRequest {
    pub fn new(args: BlockedRequestArgs) -> Self {
        let BlockedRequestArgs {
            host,
            reason,
            client,
            method,
            mode,
            protocol,
            decision,
            source,
            port,
        } = args;
        Self {
            host,
            reason,
            client,
            method,
            mode,
            protocol,
            decision,
            source,
            port,
            timestamp: unix_timestamp(),
        }
    }
}

pub(super) fn blocked_request_violation_log_line(entry: &BlockedRequest) -> String {
    match serde_json::to_string(entry) {
        Ok(json) => format!("{NETWORK_POLICY_VIOLATION_PREFIX} {json}"),
        Err(err) => {
            debug!("failed to serialize blocked request for violation log: {err}");
            format!(
                "{NETWORK_POLICY_VIOLATION_PREFIX} host={} reason={}",
                entry.host, entry.reason
            )
        }
    }
}

pub trait BlockedRequestObserver: Send + Sync + 'static {
    fn on_blocked_request(
        &self,
        request: BlockedRequest,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

impl<O: BlockedRequestObserver + ?Sized> BlockedRequestObserver for Arc<O> {
    fn on_blocked_request(
        &self,
        request: BlockedRequest,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            (**self).on_blocked_request(request).await;
        })
    }
}

impl<F, Fut> BlockedRequestObserver for F
where
    F: Fn(BlockedRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    fn on_blocked_request(
        &self,
        request: BlockedRequest,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            (self)(request).await;
        })
    }
}

fn unix_timestamp() -> i64 {
    Timestamp::now().as_second()
}
