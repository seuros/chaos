use chaos_realpath::AbsolutePathBuf;
use std::future::Future;
use std::pin::Pin;

use crate::unix::escalate_protocol::EscalationDecision;

/// Decides what action to take in response to an execve request from a client.
pub trait EscalationPolicy: Send + Sync {
    fn determine_action(
        &self,
        file: &AbsolutePathBuf,
        argv: &[String],
        workdir: &AbsolutePathBuf,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<EscalationDecision>> + Send + '_>>;
}
