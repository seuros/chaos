use super::render_debug_config_lines;
use super::session_all_proxy_url;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_kern::config::Constrained;
use chaos_kern::config_loader::ConfigLayerEntry;
use chaos_kern::config_loader::ConfigLayerStack;
use chaos_kern::config_loader::ConfigRequirements;
use chaos_kern::config_loader::ConfigRequirementsToml;
use chaos_kern::config_loader::ConstrainedWithSource;
use chaos_kern::config_loader::McpServerIdentity;
use chaos_kern::config_loader::McpServerRequirement;
use chaos_kern::config_loader::NetworkConstraints;
use chaos_kern::config_loader::RequirementSource;
use chaos_kern::config_loader::ResidencyRequirement;
use chaos_kern::config_loader::SandboxModeRequirement;
use chaos_kern::config_loader::Sourced;
use chaos_kern::config_loader::WebSearchModeRequirement;
use chaos_realpath::AbsolutePathBuf;
use ratatui::text::Line;
use std::collections::BTreeMap;
use toml::Value as TomlValue;

fn empty_toml_table() -> TomlValue {
    TomlValue::Table(toml::map::Map::new())
}

fn absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).expect("absolute path")
}

fn render_to_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn debug_config_suite() {
    debug_config_output_lists_all_layers_including_disabled();
    debug_config_output_lists_requirement_sources();
    debug_config_output_lists_session_flag_key_value_pairs();
    debug_config_output_normalizes_empty_web_search_mode_list();
    session_all_proxy_url_uses_socks_when_enabled();
    session_all_proxy_url_uses_http_when_socks_disabled();
}
#[cfg(test)]
fn debug_config_output_lists_all_layers_including_disabled() {
    let system_file = absolute_path("/etc/chaos/config.toml");
    let project_folder = absolute_path("/repo/.chaos");

    let layers = vec![
        ConfigLayerEntry::new(
            ConfigLayerSource::System { file: system_file },
            empty_toml_table(),
        ),
        ConfigLayerEntry::new_disabled(
            ConfigLayerSource::Project {
                dot_codex_folder: project_folder,
            },
            empty_toml_table(),
            "project is untrusted",
        ),
    ];
    let stack = ConfigLayerStack::new(
        layers,
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack");

    let rendered = render_to_text(&render_debug_config_lines(&stack));
    assert!(rendered.contains("(enabled)"));
    assert!(rendered.contains("(disabled)"));
    assert!(rendered.contains("reason: project is untrusted"));
    assert!(rendered.contains("Requirements:"));
    assert!(rendered.contains("  <none>"));
}

#[cfg(test)]
fn debug_config_output_lists_requirement_sources() {
    let requirements_file = absolute_path("/etc/chaos/requirements.toml");

    let requirements = ConfigRequirements {
        approval_policy: ConstrainedWithSource::new(
            Constrained::allow_any(ApprovalPolicy::Interactive),
            Some(RequirementSource::Unknown),
        ),
        sandbox_policy: ConstrainedWithSource::new(
            Constrained::allow_any(SandboxPolicy::new_read_only_policy()),
            Some(RequirementSource::SystemRequirementsToml {
                file: requirements_file.clone(),
            }),
        ),
        mcp_servers: Some(Sourced::new(
            BTreeMap::from([(
                "docs".to_string(),
                McpServerRequirement {
                    identity: McpServerIdentity::Command {
                        command: "chaos-mcp".to_string(),
                    },
                },
            )]),
            RequirementSource::Unknown,
        )),
        enforce_residency: ConstrainedWithSource::new(
            Constrained::allow_any(Some(ResidencyRequirement::Us)),
            Some(RequirementSource::Unknown),
        ),
        web_search_mode: ConstrainedWithSource::new(
            Constrained::allow_any(WebSearchMode::Cached),
            Some(RequirementSource::Unknown),
        ),
        network: Some(Sourced::new(
            NetworkConstraints {
                enabled: Some(true),
                allowed_domains: Some(vec!["example.com".to_string()]),
                ..Default::default()
            },
            RequirementSource::Unknown,
        )),
        ..ConfigRequirements::default()
    };

    let requirements_toml = ConfigRequirementsToml {
        allowed_approval_policies: Some(vec![ApprovalPolicy::Interactive]),
        allowed_sandbox_modes: Some(vec![SandboxModeRequirement::ReadOnly]),
        allowed_web_search_modes: Some(vec![WebSearchModeRequirement::Cached]),
        mcp_servers: Some(BTreeMap::from([(
            "docs".to_string(),
            McpServerRequirement {
                identity: McpServerIdentity::Command {
                    command: "chaos-mcp".to_string(),
                },
            },
        )])),
        apps: None,
        rules: None,
        enforce_residency: Some(ResidencyRequirement::Us),
        network: None,
    };

    let user_file = absolute_path("/home/alice/.chaos/config.toml");
    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::User { file: user_file },
            empty_toml_table(),
        )],
        requirements,
        requirements_toml,
    )
    .expect("config layer stack");

    let rendered = render_to_text(&render_debug_config_lines(&stack));
    assert!(rendered.contains("allowed_approval_policies: interactive (source: <unspecified>)"));
    assert!(
        rendered.contains(
            format!(
                "allowed_sandbox_modes: read-only (source: {})",
                requirements_file.as_path().display()
            )
            .as_str(),
        )
    );
    assert!(
        rendered.contains("allowed_web_search_modes: cached, disabled (source: <unspecified>)")
    );
    assert!(rendered.contains("mcp_servers: docs (source: <unspecified>)"));
    assert!(rendered.contains("enforce_residency: us (source: <unspecified>)"));
    assert!(rendered.contains(
        "experimental_network: enabled=true, allowed_domains=[example.com] (source: <unspecified>)"
    ));
    assert!(!rendered.contains("  - rules:"));
}
#[cfg(test)]
fn debug_config_output_lists_session_flag_key_value_pairs() {
    use chaos_test_fixtures::TEST_MODEL;
    let session_flags = toml::from_str::<TomlValue>(&format!(
        r#"
model = "{TEST_MODEL}"
[sandbox_workspace_write]
network_access = true
writable_roots = ["/tmp"]
"#
    ))
    .expect("session flags");

    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::SessionFlags,
            session_flags,
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack");

    let rendered = render_to_text(&render_debug_config_lines(&stack));
    assert!(rendered.contains("session-flags (enabled)"));
    assert!(rendered.contains(&format!("     - model = \"{TEST_MODEL}\"")));
    assert!(rendered.contains("     - sandbox_workspace_write.network_access = true"));
    assert!(rendered.contains("sandbox_workspace_write.writable_roots"));
    assert!(rendered.contains("/tmp"));
}

#[cfg(test)]
fn debug_config_output_normalizes_empty_web_search_mode_list() {
    let requirements = ConfigRequirements {
        web_search_mode: ConstrainedWithSource::new(
            Constrained::allow_any(WebSearchMode::Disabled),
            Some(RequirementSource::Unknown),
        ),
        ..ConfigRequirements::default()
    };

    let requirements_toml = ConfigRequirementsToml {
        allowed_approval_policies: None,
        allowed_sandbox_modes: None,
        allowed_web_search_modes: Some(Vec::new()),
        mcp_servers: None,
        apps: None,
        rules: None,
        enforce_residency: None,
        network: None,
    };

    let stack = ConfigLayerStack::new(Vec::new(), requirements, requirements_toml)
        .expect("config layer stack");

    let rendered = render_to_text(&render_debug_config_lines(&stack));
    assert!(rendered.contains("allowed_web_search_modes: disabled (source: <unspecified>)"));
}

#[cfg(test)]
fn session_all_proxy_url_uses_socks_when_enabled() {
    assert_eq!(
        session_all_proxy_url("127.0.0.1:3128", "127.0.0.1:8081", true),
        "socks5h://127.0.0.1:8081".to_string()
    );
}

#[cfg(test)]
fn session_all_proxy_url_uses_http_when_socks_disabled() {
    assert_eq!(
        session_all_proxy_url("127.0.0.1:3128", "127.0.0.1:8081", false),
        "http://127.0.0.1:3128".to_string()
    );
}
