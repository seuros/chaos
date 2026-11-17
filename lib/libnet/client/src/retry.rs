use crate::error::TransportError;
use chrono_machines::Exponential;
use chrono_machines::Retry;
use std::future::Future;
use std::num::NonZeroU8;
use std::time::Duration;
use tokio::time::sleep;

const RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum retries after the initial attempt. Zero sends once.
    pub max_attempts: u64,
    pub base_delay: Duration,
    pub retry_on: RetryOn,
}

impl RetryPolicy {
    fn retry(&self) -> Option<Retry> {
        let attempts = u8::try_from(self.max_attempts.saturating_add(1)).unwrap_or(u8::MAX);
        Exponential::new(
            self.base_delay.min(RETRY_MAX_DELAY),
            2.0,
            RETRY_MAX_DELAY,
            1.0,
        )
        .map(|shape| Retry::new(shape.into(), NonZeroU8::new(attempts)))
    }
}

#[derive(Debug, Clone)]
pub struct RetryOn {
    pub retry_429: bool,
    pub retry_5xx: bool,
    pub retry_transport: bool,
}

impl RetryOn {
    pub fn should_retry(&self, err: &TransportError) -> bool {
        match err {
            TransportError::Http { status, .. } => {
                (self.retry_429 && status.as_u16() == 429)
                    || (self.retry_5xx && status.is_server_error())
            }
            TransportError::Timeout | TransportError::Network(_) => self.retry_transport,
            _ => false,
        }
    }
}

pub async fn run_with_retry<T, R, F, Fut>(
    policy: RetryPolicy,
    mut make_req: impl FnMut() -> R,
    op: F,
) -> Result<T, TransportError>
where
    F: Fn(R, u64) -> Fut,
    Fut: Future<Output = Result<T, TransportError>>,
{
    let Some(mut retry) = policy.retry() else {
        return op(make_req(), 0).await;
    };

    loop {
        let req = make_req();
        match op(req, u64::from(retry.failures())).await {
            Ok(resp) => return Ok(resp),
            Err(err) if policy.retry_on.should_retry(&err) => {
                let Ok(delay) = retry.failed(chaos_ipc::backoff::draw()) else {
                    return Err(err);
                };
                sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }
}
