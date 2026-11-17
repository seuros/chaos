use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chaos_journald::ExecutionLease;
use chaos_journald::ExecutionRequest;
use chaos_journald::ExecutionResponse;
use chaos_journald::JournalClient;
use chaos_snitch::tracing::debug;
use chaos_snitch::tracing::warn;
use uuid::Uuid;

const SCOPE: &str = "cron:scheduler";
const SESSION: Uuid = Uuid::from_u128(0x6372_6f6e_3a73_6368_6564_756c_6572_0001);

pub type JournalConnector = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<JournalClient, String>> + Send>> + Send + Sync,
>;

pub(crate) struct SchedulerLease {
    connect: JournalConnector,
    client: Option<JournalClient>,
    owner_id: String,
    ttl_ms: u64,
    held: Option<ExecutionLease>,
}

impl SchedulerLease {
    pub(crate) fn new(connect: JournalConnector, ttl: Duration) -> Self {
        Self {
            connect,
            client: None,
            owner_id: format!("cron-{}-{}", std::process::id(), Uuid::new_v4()),
            ttl_ms: u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
            held: None,
        }
    }

    pub(crate) fn renew_every(&self) -> Duration {
        Duration::from_millis(self.ttl_ms / 3)
    }

    pub(crate) async fn hold(&mut self) -> Option<String> {
        let client = self.client().await?;
        let request = match self.held.take() {
            Some(lease) => ExecutionRequest::Heartbeat {
                lease,
                ttl_ms: self.ttl_ms,
            },
            None => ExecutionRequest::Claim {
                session_id: SESSION,
                scope: SCOPE.to_owned(),
                owner_id: self.owner_id.clone(),
                ttl_ms: self.ttl_ms,
            },
        };
        match client.execution(request).await {
            Ok(ExecutionResponse::Claimed { lease, .. } | ExecutionResponse::Lease(lease)) => {
                let token = lease.token.clone();
                self.held = Some(lease);
                Some(token)
            }
            Ok(other) => {
                warn!("cron scheduler lease: unexpected response {other:?}");
                None
            }
            Err(err) => {
                debug!("cron scheduler lease not held: {err}");
                None
            }
        }
    }

    pub(crate) async fn release(&mut self) {
        let (Some(lease), Some(client)) = (self.held.take(), self.client.as_ref()) else {
            return;
        };
        if let Err(err) = client.execution(ExecutionRequest::Release { lease }).await {
            warn!("cron scheduler lease release failed: {err}");
        }
    }

    async fn client(&mut self) -> Option<JournalClient> {
        if self.client.is_none() {
            match (self.connect)().await {
                Ok(client) => self.client = Some(client),
                Err(err) => {
                    warn!("cron scheduler cannot reach the journal: {err}");
                    return None;
                }
            }
        }
        self.client.clone()
    }
}
