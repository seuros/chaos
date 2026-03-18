use std::collections::HashSet;

use codex_core::config::Config;
use codex_core::plugins::AppConnectorId;

pub use codex_core::connectors::AppInfo;
pub use codex_core::connectors::connector_display_label;
pub use codex_core::connectors::list_accessible_connectors_from_mcp_tools;
pub use codex_core::connectors::list_accessible_connectors_from_mcp_tools_with_options;
pub use codex_core::connectors::list_accessible_connectors_from_mcp_tools_with_options_and_status;
pub use codex_core::connectors::list_cached_accessible_connectors_from_mcp_tools;
pub use codex_core::connectors::with_app_enabled_state;

use codex_core::connectors::filter_disallowed_connectors;
use codex_core::connectors::merge_connectors;
use codex_core::connectors::merge_plugin_apps;

pub async fn list_connectors(config: &Config) -> anyhow::Result<Vec<AppInfo>> {
    let connectors = list_accessible_connectors_from_mcp_tools(config).await?;
    Ok(with_app_enabled_state(connectors, config))
}

pub async fn list_all_connectors(_config: &Config) -> anyhow::Result<Vec<AppInfo>> {
    Ok(Vec::new())
}

pub async fn list_cached_all_connectors(_config: &Config) -> Option<Vec<AppInfo>> {
    Some(Vec::new())
}

pub async fn list_all_connectors_with_options(
    _config: &Config,
    _force_refetch: bool,
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(Vec::new())
}

pub fn connectors_for_plugin_apps(
    connectors: Vec<AppInfo>,
    plugin_apps: &[AppConnectorId],
) -> Vec<AppInfo> {
    let plugin_app_ids: HashSet<&str> = plugin_apps
        .iter()
        .map(|connector_id| connector_id.0.as_str())
        .collect();

    filter_disallowed_connectors(merge_plugin_apps(connectors, plugin_apps.to_vec()))
        .into_iter()
        .filter(|connector| plugin_app_ids.contains(connector.id.as_str()))
        .collect()
}

pub fn merge_connectors_with_accessible(
    connectors: Vec<AppInfo>,
    accessible_connectors: Vec<AppInfo>,
    all_connectors_loaded: bool,
) -> Vec<AppInfo> {
    let accessible_connectors = if all_connectors_loaded {
        let connector_ids: HashSet<&str> = connectors
            .iter()
            .map(|connector| connector.id.as_str())
            .collect();
        accessible_connectors
            .into_iter()
            .filter(|connector| connector_ids.contains(connector.id.as_str()))
            .collect()
    } else {
        accessible_connectors
    };

    let merged = merge_connectors(connectors, accessible_connectors);
    filter_disallowed_connectors(merged)
}
