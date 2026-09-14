use crate::history_cell::PlainHistoryCell;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::protocol::SessionNetworkProxyRuntime;
use chaos_kern::config::Config;
use chaos_kern::config_loader::ConfigLayerEntry;
use chaos_kern::config_loader::ConfigLayerStack;
use chaos_kern::config_loader::ConfigLayerStackOrdering;
use chaos_kern::config_loader::NetworkConstraints;
use chaos_kern::config_loader::RequirementSource;
use chaos_kern::config_loader::ResidencyRequirement;
use chaos_kern::config_loader::SandboxModeRequirement;
use chaos_kern::config_loader::WebSearchModeRequirement;
use ratatui::style::Stylize;
use ratatui::text::Line;
use toml::Value as TomlValue;

pub fn new_debug_config_output(
    config: &Config,
    session_network_proxy: Option<&SessionNetworkProxyRuntime>,
) -> PlainHistoryCell {
    let mut lines = render_debug_config_lines(&config.config_layer_stack);

    if let Some(proxy) = session_network_proxy {
        lines.push("".into());
        lines.push("Session runtime:".bold().into());
        lines.push("  - network_proxy".into());
        let SessionNetworkProxyRuntime {
            http_addr,
            socks_addr,
        } = proxy;
        let all_proxy = session_all_proxy_url(
            http_addr,
            socks_addr,
            config
                .permissions
                .network
                .as_ref()
                .is_some_and(chaos_kern::config::NetworkProxySpec::socks_enabled),
        );
        lines.push(format!("    - HTTP_PROXY  = http://{http_addr}").into());
        lines.push(format!("    - ALL_PROXY   = {all_proxy}").into());
    }

    PlainHistoryCell::new(lines)
}

fn session_all_proxy_url(http_addr: &str, socks_addr: &str, socks_enabled: bool) -> String {
    if socks_enabled {
        format!("socks5h://{socks_addr}")
    } else {
        format!("http://{http_addr}")
    }
}

fn render_debug_config_lines(stack: &ConfigLayerStack) -> Vec<Line<'static>> {
    let mut lines = vec![
        "/debug-config".fg(crate::theme::annotation_color()).into(),
        "".into(),
    ];

    lines.push(
        "Config layer stack (lowest precedence first):"
            .bold()
            .into(),
    );
    let layers = stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ true,
    );
    if layers.is_empty() {
        lines.push("  <none>".dim().into());
    } else {
        for (index, layer) in layers.iter().enumerate() {
            let source = format_config_layer_source(&layer.name);
            let status = if layer.is_disabled() {
                "disabled"
            } else {
                "enabled"
            };
            lines.push(format!("  {}. {source} ({status})", index + 1).into());
            lines.extend(render_non_file_layer_details(layer));
            if let Some(reason) = &layer.disabled_reason {
                lines.push(format!("     reason: {reason}").dim().into());
            }
        }
    }

    let requirements = stack.requirements();
    let requirements_toml = stack.requirements_toml();

    lines.push("".into());
    lines.push("Requirements:".bold().into());
    let mut requirement_lines = Vec::new();

    if let Some(policies) = requirements_toml.allowed_approval_policies.as_ref() {
        let value = join_or_empty(policies.iter().map(ToString::to_string).collect::<Vec<_>>());
        requirement_lines.push(requirement_line(
            "allowed_approval_policies",
            value,
            requirements.approval_policy.source.as_ref(),
        ));
    }

    if let Some(modes) = requirements_toml.allowed_sandbox_modes.as_ref() {
        let value = join_or_empty(
            modes
                .iter()
                .copied()
                .map(format_sandbox_mode_requirement)
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "allowed_sandbox_modes",
            value,
            requirements.sandbox_policy.source.as_ref(),
        ));
    }

    if let Some(modes) = requirements_toml.allowed_web_search_modes.as_ref() {
        let normalized = normalize_allowed_web_search_modes(modes);
        let value = join_or_empty(
            normalized
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "allowed_web_search_modes",
            value,
            requirements.web_search_mode.source.as_ref(),
        ));
    }

    if let Some(servers) = requirements_toml.mcp_servers.as_ref() {
        let value = join_or_empty(servers.keys().cloned().collect::<Vec<_>>());
        requirement_lines.push(requirement_line(
            "mcp_servers",
            value,
            requirements
                .mcp_servers
                .as_ref()
                .map(|sourced| &sourced.source),
        ));
    }

    // TODO(gt): Expand this debug output with detailed skills and rules display.
    if requirements_toml.rules.is_some() {
        requirement_lines.push(requirement_line(
            "rules",
            "configured".to_string(),
            requirements.exec_policy_source(),
        ));
    }

    if let Some(residency) = requirements_toml.enforce_residency {
        requirement_lines.push(requirement_line(
            "enforce_residency",
            format_residency_requirement(residency),
            requirements.enforce_residency.source.as_ref(),
        ));
    }

    if let Some(network) = requirements.network.as_ref() {
        requirement_lines.push(requirement_line(
            "experimental_network",
            format_network_constraints(&network.value),
            Some(&network.source),
        ));
    }

    if requirement_lines.is_empty() {
        lines.push("  <none>".dim().into());
    } else {
        lines.extend(requirement_lines);
    }

    lines
}

fn render_non_file_layer_details(layer: &ConfigLayerEntry) -> Vec<Line<'static>> {
    match &layer.name {
        ConfigLayerSource::SessionFlags => render_session_flag_details(&layer.config),
        ConfigLayerSource::System { .. }
        | ConfigLayerSource::Bootstrap { .. }
        | ConfigLayerSource::User { .. }
        | ConfigLayerSource::UserDatabase { .. }
        | ConfigLayerSource::ProjectMcp { .. }
        | ConfigLayerSource::Project { .. } => Vec::new(),
    }
}

fn render_session_flag_details(config: &TomlValue) -> Vec<Line<'static>> {
    let mut pairs = Vec::new();
    flatten_toml_key_values(config, /*prefix*/ None, &mut pairs);

    if pairs.is_empty() {
        return vec!["     - <none>".dim().into()];
    }

    pairs
        .into_iter()
        .map(|(key, value)| format!("     - {key} = {value}").into())
        .collect()
}

fn flatten_toml_key_values(
    value: &TomlValue,
    prefix: Option<&str>,
    out: &mut Vec<(String, String)>,
) {
    match value {
        TomlValue::Table(table) => {
            let mut entries = table.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| key.as_str());
            for (key, child) in entries {
                let next_prefix = if let Some(prefix) = prefix {
                    format!("{prefix}.{key}")
                } else {
                    key.to_string()
                };
                flatten_toml_key_values(child, Some(&next_prefix), out);
            }
        }
        _ => {
            let key = prefix.unwrap_or("<value>").to_string();
            out.push((key, format_toml_value(value)));
        }
    }
}

fn format_toml_value(value: &TomlValue) -> String {
    value.to_string()
}

fn requirement_line(
    name: &str,
    value: String,
    source: Option<&RequirementSource>,
) -> Line<'static> {
    let source = source
        .map(ToString::to_string)
        .unwrap_or_else(|| "<unspecified>".to_string());
    format!("  - {name}: {value} (source: {source})").into()
}

fn join_or_empty(values: Vec<String>) -> String {
    if values.is_empty() {
        "<empty>".to_string()
    } else {
        values.join(", ")
    }
}

fn normalize_allowed_web_search_modes(
    modes: &[WebSearchModeRequirement],
) -> Vec<WebSearchModeRequirement> {
    if modes.is_empty() {
        return vec![WebSearchModeRequirement::Disabled];
    }

    let mut normalized = modes.to_vec();
    if !normalized.contains(&WebSearchModeRequirement::Disabled) {
        normalized.push(WebSearchModeRequirement::Disabled);
    }
    normalized
}

fn format_config_layer_source(source: &ConfigLayerSource) -> String {
    match source {
        ConfigLayerSource::System { file } => {
            format!("system ({})", file.as_path().display())
        }
        ConfigLayerSource::Bootstrap { file } => {
            format!("bootstrap ({})", file.as_path().display())
        }
        ConfigLayerSource::User { file } => {
            format!("user ({})", file.as_path().display())
        }
        ConfigLayerSource::UserDatabase { revision } => {
            format!("user database (revision {revision})")
        }
        ConfigLayerSource::Project { dot_codex_folder } => {
            format!(
                "project ({}/config.toml)",
                dot_codex_folder.as_path().display()
            )
        }
        ConfigLayerSource::ProjectMcp { file } => {
            format!("project ({})", file.as_path().display())
        }
        ConfigLayerSource::SessionFlags => "session-flags".to_string(),
    }
}

fn format_sandbox_mode_requirement(mode: SandboxModeRequirement) -> String {
    use chaos_ipc::config_types::{
        SANDBOX_MODE_READ_ONLY, SANDBOX_MODE_ROOT_ACCESS, SANDBOX_MODE_WORKSPACE_WRITE,
    };
    match mode {
        SandboxModeRequirement::ReadOnly => SANDBOX_MODE_READ_ONLY.to_string(),
        SandboxModeRequirement::WorkspaceWrite => SANDBOX_MODE_WORKSPACE_WRITE.to_string(),
        SandboxModeRequirement::RootAccess => SANDBOX_MODE_ROOT_ACCESS.to_string(),
        SandboxModeRequirement::ExternalSandbox => "external-sandbox".to_string(),
    }
}

fn format_residency_requirement(requirement: ResidencyRequirement) -> String {
    match requirement {
        ResidencyRequirement::Us => "us".to_string(),
    }
}

fn format_network_constraints(network: &NetworkConstraints) -> String {
    let mut parts = Vec::new();

    let NetworkConstraints {
        enabled,
        http_port,
        socks_port,
        allow_upstream_proxy,
        dangerously_allow_non_loopback_proxy,
        dangerously_allow_all_unix_sockets,
        allowed_domains,
        managed_allowed_domains_only,
        denied_domains,
        allow_unix_sockets,
        allow_local_binding,
    } = network;

    if let Some(enabled) = enabled {
        parts.push(format!("enabled={enabled}"));
    }
    if let Some(http_port) = http_port {
        parts.push(format!("http_port={http_port}"));
    }
    if let Some(socks_port) = socks_port {
        parts.push(format!("socks_port={socks_port}"));
    }
    if let Some(allow_upstream_proxy) = allow_upstream_proxy {
        parts.push(format!("allow_upstream_proxy={allow_upstream_proxy}"));
    }
    if let Some(dangerously_allow_non_loopback_proxy) = dangerously_allow_non_loopback_proxy {
        parts.push(format!(
            "dangerously_allow_non_loopback_proxy={dangerously_allow_non_loopback_proxy}"
        ));
    }
    if let Some(dangerously_allow_all_unix_sockets) = dangerously_allow_all_unix_sockets {
        parts.push(format!(
            "dangerously_allow_all_unix_sockets={dangerously_allow_all_unix_sockets}"
        ));
    }
    if let Some(allowed_domains) = allowed_domains {
        parts.push(format!("allowed_domains=[{}]", allowed_domains.join(", ")));
    }
    if let Some(managed_allowed_domains_only) = managed_allowed_domains_only {
        parts.push(format!(
            "managed_allowed_domains_only={managed_allowed_domains_only}"
        ));
    }
    if let Some(denied_domains) = denied_domains {
        parts.push(format!("denied_domains=[{}]", denied_domains.join(", ")));
    }
    if let Some(allow_unix_sockets) = allow_unix_sockets {
        parts.push(format!(
            "allow_unix_sockets=[{}]",
            allow_unix_sockets.join(", ")
        ));
    }
    if let Some(allow_local_binding) = allow_local_binding {
        parts.push(format!("allow_local_binding={allow_local_binding}"));
    }

    join_or_empty(parts)
}

#[cfg(test)]
pub(crate) mod tests;
