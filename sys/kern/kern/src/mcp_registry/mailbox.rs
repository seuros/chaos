use std::fmt::Display;

use super::*;

pub(super) async fn admit<Command: Send>(
    mailbox: &Adapter<Command>,
    command: Command,
    timeout_context: &'static str,
    unavailable_context: &'static str,
) -> anyhow::Result<()> {
    tokio::time::timeout(MCP_MAILBOX_TIMEOUT, mailbox.send(command))
        .await
        .context(timeout_context)?
        .context(unavailable_context)
}

impl McpRegistryLifecycle {
    pub(super) fn running_reply<T>(
        &self,
        reply: oneshot::Sender<anyhow::Result<T>>,
        rejection: impl Display,
    ) -> Option<oneshot::Sender<anyhow::Result<T>>> {
        if self.state.load(Ordering::Acquire) == REGISTRY_RUNNING {
            return Some(reply);
        }
        // The caller may already have cancelled its wait during shutdown.
        let _ = reply.send(Err(anyhow::anyhow!(
            "MCP registry is shutting down; {rejection}"
        )));
        None
    }
}
