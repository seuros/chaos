use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

pub use codex_app_server_protocol::AppBranding;
pub use codex_app_server_protocol::AppInfo;
pub use codex_app_server_protocol::AppMetadata;
use rmcp::model::ToolAnnotations;
use serde::Deserialize;

use crate::CodexAuth;
use crate::config::Config;
use crate::config::types::AppToolApproval;
use crate::config::types::AppsConfigToml;
use crate::config_loader::AppsRequirementsToml;
use crate::default_client::is_first_party_chat_originator;
use crate::default_client::originator;
use crate::mcp::CODEX_APPS_MCP_SERVER_NAME;
use crate::mcp::ToolPluginProvenance;
use crate::plugins::AppConnectorId;

pub const CONNECTORS_CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppToolPolicy {
    pub enabled: bool,
    pub approval: AppToolApproval,
}

impl Default for AppToolPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            approval: AppToolApproval::Auto,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccessibleConnectorsStatus {
    pub connectors: Vec<AppInfo>,
    pub codex_apps_ready: bool,
}

pub async fn list_accessible_connectors_from_mcp_tools(
    _config: &Config,
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(Vec::new())
}

pub(crate) async fn list_tool_suggest_discoverable_tools_with_auth(
    _config: &Config,
    _auth: Option<&CodexAuth>,
    _accessible_connectors: &[AppInfo],
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(Vec::new())
}

pub async fn list_cached_accessible_connectors_from_mcp_tools(
    _config: &Config,
) -> Option<Vec<AppInfo>> {
    Some(Vec::new())
}

pub(crate) fn refresh_accessible_connectors_cache_from_mcp_tools(
    _config: &Config,
    _auth: Option<&CodexAuth>,
    _mcp_tools: &HashMap<String, crate::mcp_connection_manager::ToolInfo>,
) {
}

pub async fn list_accessible_connectors_from_mcp_tools_with_options(
    _config: &Config,
    _force_refetch: bool,
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(Vec::new())
}

pub async fn list_accessible_connectors_from_mcp_tools_with_options_and_status(
    _config: &Config,
    _force_refetch: bool,
) -> anyhow::Result<AccessibleConnectorsStatus> {
    Ok(AccessibleConnectorsStatus {
        connectors: Vec::new(),
        codex_apps_ready: true,
    })
}

pub fn connector_display_label(connector: &AppInfo) -> String {
    format_connector_label(&connector.name, &connector.id)
}

pub fn connector_mention_slug(connector: &AppInfo) -> String {
    sanitize_slug(&connector_display_label(connector))
}

pub(crate) fn accessible_connectors_from_mcp_tools(
    mcp_tools: &HashMap<String, crate::mcp_connection_manager::ToolInfo>,
) -> Vec<AppInfo> {
    let tools = mcp_tools.values().filter_map(|tool| {
        if tool.server_name != CODEX_APPS_MCP_SERVER_NAME {
            return None;
        }
        let connector_id = tool.connector_id.as_deref()?;
        Some((
            connector_id.to_string(),
            normalize_connector_value(tool.connector_name.as_deref()),
            normalize_connector_value(tool.connector_description.as_deref()),
            tool.plugin_display_names.clone(),
        ))
    });
    collect_accessible_connectors(tools)
}

pub fn merge_connectors(
    connectors: Vec<AppInfo>,
    accessible_connectors: Vec<AppInfo>,
) -> Vec<AppInfo> {
    let mut merged: HashMap<String, AppInfo> = connectors
        .into_iter()
        .map(|mut connector| {
            connector.is_accessible = false;
            (connector.id.clone(), connector)
        })
        .collect();

    for mut connector in accessible_connectors {
        connector.is_accessible = true;
        let connector_id = connector.id.clone();
        if let Some(existing) = merged.get_mut(&connector_id) {
            existing.is_accessible = true;
            if existing.name == existing.id && connector.name != connector.id {
                existing.name = connector.name;
            }
            if existing.description.is_none() && connector.description.is_some() {
                existing.description = connector.description;
            }
            if existing.logo_url.is_none() && connector.logo_url.is_some() {
                existing.logo_url = connector.logo_url;
            }
            if existing.logo_url_dark.is_none() && connector.logo_url_dark.is_some() {
                existing.logo_url_dark = connector.logo_url_dark;
            }
            if existing.distribution_channel.is_none() && connector.distribution_channel.is_some() {
                existing.distribution_channel = connector.distribution_channel;
            }
            existing
                .plugin_display_names
                .extend(connector.plugin_display_names);
        } else {
            merged.insert(connector_id, connector);
        }
    }

    let mut merged = merged.into_values().collect::<Vec<_>>();
    for connector in &mut merged {
        if connector.install_url.is_none() {
            connector.install_url = Some(connector_install_url(&connector.name, &connector.id));
        }
        connector.plugin_display_names.sort_unstable();
        connector.plugin_display_names.dedup();
    }
    merged.sort_by(|left, right| {
        right
            .is_accessible
            .cmp(&left.is_accessible)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    merged
}

pub fn merge_plugin_apps(
    connectors: Vec<AppInfo>,
    plugin_apps: Vec<AppConnectorId>,
) -> Vec<AppInfo> {
    let mut merged = connectors;
    let mut connector_ids = merged
        .iter()
        .map(|connector| connector.id.clone())
        .collect::<HashSet<_>>();

    for connector_id in plugin_apps {
        if connector_ids.insert(connector_id.0.clone()) {
            merged.push(plugin_app_to_app_info(connector_id));
        }
    }

    merged.sort_by(|left, right| {
        right
            .is_accessible
            .cmp(&left.is_accessible)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    merged
}

pub fn merge_plugin_apps_with_accessible(
    plugin_apps: Vec<AppConnectorId>,
    accessible_connectors: Vec<AppInfo>,
) -> Vec<AppInfo> {
    let accessible_connector_ids: HashSet<&str> = accessible_connectors
        .iter()
        .map(|connector| connector.id.as_str())
        .collect();
    let plugin_connectors = plugin_apps
        .into_iter()
        .filter(|connector_id| accessible_connector_ids.contains(connector_id.0.as_str()))
        .map(plugin_app_to_app_info)
        .collect::<Vec<_>>();
    merge_connectors(plugin_connectors, accessible_connectors)
}

pub fn with_app_enabled_state(mut connectors: Vec<AppInfo>, config: &Config) -> Vec<AppInfo> {
    let user_apps_config = read_user_apps_config(config);
    let requirements_apps_config = config.config_layer_stack.requirements_toml().apps.as_ref();
    if user_apps_config.is_none() && requirements_apps_config.is_none() {
        return connectors;
    }

    for connector in &mut connectors {
        if let Some(apps_config) = user_apps_config.as_ref()
            && (apps_config.default.is_some()
                || apps_config.apps.contains_key(connector.id.as_str()))
        {
            connector.is_enabled = app_is_enabled(apps_config, Some(connector.id.as_str()));
        }

        if requirements_apps_config
            .and_then(|apps| apps.apps.get(connector.id.as_str()))
            .is_some_and(|app| app.enabled == Some(false))
        {
            connector.is_enabled = false;
        }
    }

    connectors
}

pub fn with_app_plugin_sources(
    mut connectors: Vec<AppInfo>,
    tool_plugin_provenance: &ToolPluginProvenance,
) -> Vec<AppInfo> {
    for connector in &mut connectors {
        connector.plugin_display_names = tool_plugin_provenance
            .plugin_display_names_for_connector_id(connector.id.as_str())
            .to_vec();
    }
    connectors
}

pub(crate) fn app_tool_policy(
    config: &Config,
    connector_id: Option<&str>,
    tool_name: &str,
    tool_title: Option<&str>,
    annotations: Option<&ToolAnnotations>,
) -> AppToolPolicy {
    let apps_config = read_apps_config(config);
    app_tool_policy_from_apps_config(
        apps_config.as_ref(),
        connector_id,
        tool_name,
        tool_title,
        annotations,
    )
}

pub(crate) fn codex_app_tool_is_enabled(
    config: &Config,
    tool_info: &crate::mcp_connection_manager::ToolInfo,
) -> bool {
    if tool_info.server_name != CODEX_APPS_MCP_SERVER_NAME {
        return true;
    }

    app_tool_policy(
        config,
        tool_info.connector_id.as_deref(),
        &tool_info.tool.name,
        tool_info.tool.title.as_deref(),
        tool_info.tool.annotations.as_ref(),
    )
    .enabled
}

const DISALLOWED_CONNECTOR_IDS: &[&str] = &[
    "asdk_app_6938a94a61d881918ef32cb999ff937c",
    "connector_2b0a9009c9c64bf9933a3dae3f2b1254",
    "connector_68de829bf7648191acd70a907364c67c",
    "connector_68e004f14af881919eb50893d3d9f523",
    "connector_69272cb413a081919685ec3c88d1744e",
];
const FIRST_PARTY_CHAT_DISALLOWED_CONNECTOR_IDS: &[&str] =
    &["connector_0f9c9d4592e54d0a9a12b3f44a1e2010"];
const DISALLOWED_CONNECTOR_PREFIX: &str = "connector_openai_";

pub fn filter_disallowed_connectors(connectors: Vec<AppInfo>) -> Vec<AppInfo> {
    filter_disallowed_connectors_for_originator(connectors, originator().value.as_str())
}

pub(crate) fn is_connector_id_allowed(connector_id: &str) -> bool {
    is_connector_id_allowed_for_originator(connector_id, originator().value.as_str())
}

fn filter_disallowed_connectors_for_originator(
    connectors: Vec<AppInfo>,
    originator_value: &str,
) -> Vec<AppInfo> {
    connectors
        .into_iter()
        .filter(|connector| {
            is_connector_id_allowed_for_originator(connector.id.as_str(), originator_value)
        })
        .collect()
}

fn is_connector_id_allowed_for_originator(connector_id: &str, originator_value: &str) -> bool {
    let disallowed_connector_ids = if is_first_party_chat_originator(originator_value) {
        FIRST_PARTY_CHAT_DISALLOWED_CONNECTOR_IDS
    } else {
        DISALLOWED_CONNECTOR_IDS
    };

    !connector_id.starts_with(DISALLOWED_CONNECTOR_PREFIX)
        && !disallowed_connector_ids.contains(&connector_id)
}

fn read_apps_config(config: &Config) -> Option<AppsConfigToml> {
    let apps_config = read_user_apps_config(config);
    let had_apps_config = apps_config.is_some();
    let mut apps_config = apps_config.unwrap_or_default();
    apply_requirements_apps_constraints(
        &mut apps_config,
        config.config_layer_stack.requirements_toml().apps.as_ref(),
    );
    if had_apps_config || apps_config.default.is_some() || !apps_config.apps.is_empty() {
        Some(apps_config)
    } else {
        None
    }
}

fn read_user_apps_config(config: &Config) -> Option<AppsConfigToml> {
    config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("apps"))
        .cloned()
        .and_then(|value| AppsConfigToml::deserialize(value).ok())
}

fn apply_requirements_apps_constraints(
    apps_config: &mut AppsConfigToml,
    requirements_apps_config: Option<&AppsRequirementsToml>,
) {
    let Some(requirements_apps_config) = requirements_apps_config else {
        return;
    };

    for (app_id, requirement) in &requirements_apps_config.apps {
        if requirement.enabled != Some(false) {
            continue;
        }
        let app = apps_config.apps.entry(app_id.clone()).or_default();
        app.enabled = false;
    }
}

fn app_is_enabled(apps_config: &AppsConfigToml, connector_id: Option<&str>) -> bool {
    let default_enabled = apps_config
        .default
        .as_ref()
        .map(|defaults| defaults.enabled)
        .unwrap_or(true);

    connector_id
        .and_then(|connector_id| apps_config.apps.get(connector_id))
        .map(|app| app.enabled)
        .unwrap_or(default_enabled)
}

fn app_tool_policy_from_apps_config(
    apps_config: Option<&AppsConfigToml>,
    connector_id: Option<&str>,
    tool_name: &str,
    tool_title: Option<&str>,
    annotations: Option<&ToolAnnotations>,
) -> AppToolPolicy {
    let Some(apps_config) = apps_config else {
        return AppToolPolicy::default();
    };

    let app = connector_id.and_then(|connector_id| apps_config.apps.get(connector_id));
    let tools = app.and_then(|app| app.tools.as_ref());
    let tool_config = tools.and_then(|tools| {
        tools
            .tools
            .get(tool_name)
            .or_else(|| tool_title.and_then(|title| tools.tools.get(title)))
    });
    let approval = tool_config
        .and_then(|tool| tool.approval_mode)
        .or_else(|| app.and_then(|app| app.default_tools_approval_mode))
        .unwrap_or(AppToolApproval::Auto);

    if !app_is_enabled(apps_config, connector_id) {
        return AppToolPolicy {
            enabled: false,
            approval,
        };
    }

    if let Some(enabled) = tool_config.and_then(|tool| tool.enabled) {
        return AppToolPolicy { enabled, approval };
    }

    if let Some(enabled) = app.and_then(|app| app.default_tools_enabled) {
        return AppToolPolicy { enabled, approval };
    }

    let app_defaults = apps_config.default.as_ref();
    let destructive_enabled = app
        .and_then(|app| app.destructive_enabled)
        .unwrap_or_else(|| {
            app_defaults
                .map(|defaults| defaults.destructive_enabled)
                .unwrap_or(true)
        });
    let open_world_enabled = app
        .and_then(|app| app.open_world_enabled)
        .unwrap_or_else(|| {
            app_defaults
                .map(|defaults| defaults.open_world_enabled)
                .unwrap_or(true)
        });
    let destructive_hint = annotations
        .and_then(|annotations| annotations.destructive_hint)
        .unwrap_or(false);
    let open_world_hint = annotations
        .and_then(|annotations| annotations.open_world_hint)
        .unwrap_or(false);
    let enabled =
        (destructive_enabled || !destructive_hint) && (open_world_enabled || !open_world_hint);

    AppToolPolicy { enabled, approval }
}

fn collect_accessible_connectors<I>(tools: I) -> Vec<AppInfo>
where
    I: IntoIterator<Item = (String, Option<String>, Option<String>, Vec<String>)>,
{
    let mut connectors: HashMap<String, (AppInfo, BTreeSet<String>)> = HashMap::new();
    for (connector_id, connector_name, connector_description, plugin_display_names) in tools {
        let connector_name = connector_name.unwrap_or_else(|| connector_id.clone());
        if let Some((existing, existing_plugin_display_names)) = connectors.get_mut(&connector_id) {
            if existing.name == connector_id && connector_name != connector_id {
                existing.name = connector_name;
            }
            if existing.description.is_none() && connector_description.is_some() {
                existing.description = connector_description;
            }
            existing_plugin_display_names.extend(plugin_display_names);
        } else {
            connectors.insert(
                connector_id.clone(),
                (
                    AppInfo {
                        id: connector_id.clone(),
                        name: connector_name,
                        description: connector_description,
                        logo_url: None,
                        logo_url_dark: None,
                        distribution_channel: None,
                        branding: None,
                        app_metadata: None,
                        labels: None,
                        install_url: None,
                        is_accessible: true,
                        is_enabled: true,
                        plugin_display_names: Vec::new(),
                    },
                    plugin_display_names
                        .into_iter()
                        .collect::<BTreeSet<String>>(),
                ),
            );
        }
    }
    let mut accessible: Vec<AppInfo> = connectors
        .into_values()
        .map(|(mut connector, plugin_display_names)| {
            connector.plugin_display_names = plugin_display_names.into_iter().collect();
            connector.install_url = Some(connector_install_url(&connector.name, &connector.id));
            connector
        })
        .collect();
    accessible.sort_by(|left, right| {
        right
            .is_accessible
            .cmp(&left.is_accessible)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    accessible
}

fn plugin_app_to_app_info(connector_id: AppConnectorId) -> AppInfo {
    let connector_id = connector_id.0;
    let name = connector_id.clone();
    AppInfo {
        id: connector_id.clone(),
        name: name.clone(),
        description: None,
        logo_url: None,
        logo_url_dark: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: Some(connector_install_url(&name, &connector_id)),
        is_accessible: false,
        is_enabled: true,
        plugin_display_names: Vec::new(),
    }
}

fn normalize_connector_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn connector_install_url(name: &str, connector_id: &str) -> String {
    let slug = sanitize_slug(name);
    format!("https://chatgpt.com/apps/{slug}/{connector_id}")
}

pub fn sanitize_name(name: &str) -> String {
    sanitize_slug(name).replace("-", "_")
}

fn sanitize_slug(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
        } else {
            normalized.push('-');
        }
    }
    let normalized = normalized.trim_matches('-');
    if normalized.is_empty() {
        "app".to_string()
    } else {
        normalized.to_string()
    }
}

fn format_connector_label(name: &str, _id: &str) -> String {
    name.to_string()
}
