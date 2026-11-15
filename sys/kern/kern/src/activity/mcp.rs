//! An MCP server's state as the bus carries it: never its error text.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpState {
    Starting,
    Ready,
    Failed,
    Cancelled,
}

impl From<&chaos_ipc::protocol::McpStartupStatus> for McpState {
    fn from(status: &chaos_ipc::protocol::McpStartupStatus) -> Self {
        use chaos_ipc::protocol::McpStartupStatus as Status;
        match status {
            Status::Starting => Self::Starting,
            Status::Ready => Self::Ready,
            Status::Failed { .. } => Self::Failed,
            Status::Cancelled => Self::Cancelled,
        }
    }
}
