//! Remote task observation, independent of model turns and conversation state.

use crate::McpTask;
use chaos_sysctl::types::McpServerTransportConfig;
use chrono_machines::Exponential;
use chrono_machines::Retry;
use mcp_guest::protocol::TaskStatus;
use std::future::Future;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const UNAVAILABLE_MAX_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum TaskObservation {
    Status(McpTask),
    Unavailable(String),
    /// The server explicitly no longer recognizes the saved handle.
    Lost(String),
}

pub fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

/// Non-secret endpoint binding. Credentials are deliberately excluded, allowing
/// credential refresh without treating a different endpoint as the same task.
pub fn endpoint_identity(transport: &McpServerTransportConfig) -> String {
    let (kind, identity) = match transport {
        McpServerTransportConfig::Stdio {
            command, args, cwd, ..
        } => ("stdio", format!("{command:?}:{args:?}:{cwd:?}")),
        McpServerTransportConfig::StreamableHttp { url, .. } => ("http", url.clone()),
    };
    format!(
        "{kind}:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, identity.as_bytes())
    )
}

/// The caller supplies transport admission so polling always uses the current
/// connection and its existing circuit breaker. No tool is resubmitted here.
pub fn observe_task<F, Fut>(
    initial: McpTask,
    cancel: CancellationToken,
    mut fetch: F,
) -> mpsc::Receiver<TaskObservation>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<McpTask>> + Send,
{
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut delay = poll_delay(&initial);
        let mut unavailable = unavailable_retry(delay);
        let mut terminal = is_terminal(initial.status);
        let mut next = TaskObservation::Status(initial);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                result = tx.send(next) => if result.is_err() { break; },
            }
            if terminal {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(delay) => {}
            }
            let fetched = tokio::select! {
                _ = cancel.cancelled() => break,
                result = tokio::time::timeout(Duration::from_secs(30), fetch()) =>
                    result.map_err(anyhow::Error::from).and_then(|value| value),
            };
            next = match fetched {
                Ok(task) => {
                    delay = poll_delay(&task);
                    unavailable = unavailable_retry(delay);
                    terminal = is_terminal(task.status);
                    TaskObservation::Status(task)
                }
                Err(error) => {
                    if matches!(
                        error.downcast_ref::<mcp_guest::GuestError>(),
                        Some(mcp_guest::GuestError::Server { code: -32602, .. })
                    ) {
                        terminal = true;
                        TaskObservation::Lost(error.to_string())
                    } else {
                        delay = unavailable
                            .as_mut()
                            .and_then(|retry| retry.failed(0).ok())
                            .unwrap_or(UNAVAILABLE_MAX_DELAY);
                        TaskObservation::Unavailable(error.to_string())
                    }
                }
            };
        }
    });
    rx
}

fn unavailable_retry(poll: Duration) -> Option<Retry> {
    Exponential::new(
        poll.saturating_mul(2).min(UNAVAILABLE_MAX_DELAY),
        2.0,
        UNAVAILABLE_MAX_DELAY,
        0.0,
    )
    .map(|shape| Retry::new(shape.into(), None))
}

fn poll_delay(task: &McpTask) -> Duration {
    Duration::from_millis(task.poll_interval.unwrap_or(1000).clamp(250, 60_000))
}

#[cfg(test)]
mod tests;
