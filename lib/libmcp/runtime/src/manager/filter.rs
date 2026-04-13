use std::collections::HashSet;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use chaos_sysctl::types::McpServerConfig;

use super::ToolInfo;

/// A tool is allowed to be used if both are true:
/// 1. enabled is None (no allowlist is set) or the tool is explicitly enabled.
/// 2. The tool is not explicitly disabled.
#[derive(Default, Clone)]
pub struct ToolFilter {
    pub(crate) enabled: Option<HashSet<String>>,
    pub(crate) disabled: HashSet<String>,
}

impl ToolFilter {
    pub(super) fn from_config(cfg: &McpServerConfig) -> Self {
        let enabled = cfg
            .enabled_tools
            .as_ref()
            .map(|tools| tools.iter().cloned().collect::<HashSet<_>>());
        let disabled = cfg
            .disabled_tools
            .as_ref()
            .map(|tools| tools.iter().cloned().collect::<HashSet<_>>())
            .unwrap_or_default();

        Self { enabled, disabled }
    }

    pub fn allows(&self, tool_name: &str) -> bool {
        if let Some(enabled) = &self.enabled
            && !enabled.contains(tool_name)
        {
            return false;
        }

        !self.disabled.contains(tool_name)
    }
}

pub(super) fn filter_tools(tools: Vec<ToolInfo>, filter: &ToolFilter) -> Vec<ToolInfo> {
    tools
        .into_iter()
        .filter(|tool| filter.allows(&tool.tool.name))
        .collect()
}

pub(super) fn store_managed_tools(
    tool_filter: &ToolFilter,
    tools_arc: &Arc<StdRwLock<Vec<ToolInfo>>>,
    tools: Vec<ToolInfo>,
) -> Vec<ToolInfo> {
    let filtered_tools = filter_tools(tools, tool_filter);
    *tools_arc
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = filtered_tools.clone();
    filtered_tools
}

#[derive(Debug, Clone, thiserror::Error)]
pub(super) enum StartupOutcomeError {
    #[error("MCP startup cancelled")]
    Cancelled,
    // We can't store the original error here because anyhow::Error doesn't implement
    // `Clone`.
    #[error("MCP startup failed: {error}")]
    Failed { error: String },
}

impl From<anyhow::Error> for StartupOutcomeError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed {
            error: error.to_string(),
        }
    }
}
