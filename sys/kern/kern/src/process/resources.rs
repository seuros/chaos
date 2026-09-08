//! Read-only resource access for attached clients, outside the conversation.

use super::Process;
use chaos_mcp_runtime::{PaginatedRequestParams, ReadResourceRequestParams};

impl Process {
    /// Uses the process's existing MCP connections and sandbox, without creating a turn
    /// or recording resource contents in model history.
    pub async fn list_mcp_resources(
        &self,
        server: &str,
        cursor: Option<String>,
    ) -> anyhow::Result<chaos_mcp_runtime::ListResourcesResult> {
        self.chaos
            .session
            .list_resources(server, Some(PaginatedRequestParams { cursor }))
            .await
    }

    /// Read a resource for a client, not for the model.
    pub async fn read_mcp_resource(
        &self,
        server: &str,
        uri: String,
    ) -> anyhow::Result<chaos_mcp_runtime::ReadResourceResult> {
        self.chaos
            .session
            .read_resource(server, ReadResourceRequestParams { uri, meta: None })
            .await
    }
}
