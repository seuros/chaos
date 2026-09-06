//! Remote task observation, independent of model turns and conversation state.

use crate::McpTask;
use chaos_sysctl::types::McpServerTransportConfig;
use mcp_guest::protocol::TaskStatus;
use std::future::Future;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
                        delay = (delay * 2).min(Duration::from_secs(60));
                        TaskObservation::Unavailable(error.to_string())
                    }
                }
            };
        }
    });
    rx
}

fn poll_delay(task: &McpTask) -> Duration {
    Duration::from_millis(task.poll_interval.unwrap_or(1000).clamp(250, 60_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(status: TaskStatus) -> McpTask {
        McpTask {
            task_id: "owned".into(),
            status,
            status_message: None,
            created_at: "now".into(),
            last_updated_at: "now".into(),
            ttl: None,
            poll_interval: Some(1),
        }
    }

    #[tokio::test]
    async fn terminal_observation_does_not_poll_again() {
        let mut events = observe_task(
            task(TaskStatus::Completed),
            CancellationToken::new(),
            || async {
                panic!("terminal tasks must not be polled");
                #[allow(unreachable_code)]
                Ok(task(TaskStatus::Working))
            },
        );
        assert!(matches!(
            events.recv().await,
            Some(TaskObservation::Status(_))
        ));
        assert!(events.recv().await.is_none());
    }

    #[tokio::test]
    async fn observation_failure_is_not_a_task_outcome_and_cancel_stops_polling() {
        let cancel = CancellationToken::new();
        let mut events = observe_task(task(TaskStatus::Working), cancel.clone(), || async {
            anyhow::bail!("disconnected")
        });
        assert!(matches!(
            events.recv().await,
            Some(TaskObservation::Status(_))
        ));
        assert!(matches!(
            events.recv().await,
            Some(TaskObservation::Unavailable(_))
        ));
        cancel.cancel();
        assert!(events.recv().await.is_none());
    }

    #[test]
    fn polling_hints_are_bounded() {
        assert_eq!(
            poll_delay(&task(TaskStatus::Working)),
            Duration::from_millis(250)
        );
        let mut task = task(TaskStatus::Working);
        task.poll_interval = Some(u64::MAX);
        assert_eq!(poll_delay(&task), Duration::from_secs(60));
    }
}
