use super::*;

use chaos_selinux::Decision;
use chaos_selinux::NetworkRuleProtocol;
use chaos_selinux::Policy;
use pretty_assertions::assert_eq;

#[test]
fn higher_precedence_profile_network_beats_lower_profile_network() {
    let lower_network: toml::Value = toml::from_str(
        r#"
default_permissions = "workspace"

[permissions.workspace.network]
allowed_domains = ["lower.example.com"]
"#,
    )
    .expect("lower layer should parse");
    let higher_network: toml::Value = toml::from_str(
        r#"
default_permissions = "workspace"

[permissions.workspace.network]
allowed_domains = ["higher.example.com"]
"#,
    )
    .expect("higher layer should parse");

    let mut config = NetworkProxyConfig::default();
    apply_network_tables(
        &mut config,
        network_tables_from_toml(&lower_network).expect("lower layer should deserialize"),
    )
    .expect("lower layer should apply");
    apply_network_tables(
        &mut config,
        network_tables_from_toml(&higher_network).expect("higher layer should deserialize"),
    )
    .expect("higher layer should apply");

    assert_eq!(config.network.allowed_domains, vec!["higher.example.com"]);
}

#[test]
fn execpolicy_network_rules_overlay_network_lists() {
    let mut config = NetworkProxyConfig::default();
    config.network.allowed_domains = vec!["config.example.com".to_string()];
    config.network.denied_domains = vec!["blocked.example.com".to_string()];

    let mut exec_policy = Policy::empty();
    exec_policy
        .add_network_rule(
            "blocked.example.com",
            NetworkRuleProtocol::Https,
            Decision::Allow,
            None,
        )
        .expect("allow rule should be valid");
    exec_policy
        .add_network_rule(
            "api.example.com",
            NetworkRuleProtocol::Http,
            Decision::Forbidden,
            None,
        )
        .expect("deny rule should be valid");

    apply_exec_policy_network_rules(&mut config, &exec_policy);

    assert_eq!(
        config.network.allowed_domains,
        vec![
            "config.example.com".to_string(),
            "blocked.example.com".to_string()
        ]
    );
    assert_eq!(
        config.network.denied_domains,
        vec!["api.example.com".to_string()]
    );
}

#[test]
fn requirements_network_maps_to_proxy_constraints() {
    let network = NetworkConstraints {
        enabled: Some(true),
        allow_upstream_proxy: Some(false),
        dangerously_allow_non_loopback_proxy: Some(true),
        dangerously_allow_all_unix_sockets: Some(true),
        allowed_domains: Some(vec!["example.com".to_string()]),
        denied_domains: Some(vec!["blocked.example.com".to_string()]),
        allow_unix_sockets: Some(vec!["/run/docker.sock".to_string()]),
        allow_local_binding: Some(true),
        ..NetworkConstraints::default()
    };

    let constraints = constraints_from_requirements(Some(&network));

    assert_eq!(constraints.enabled, Some(true));
    assert_eq!(constraints.mode, None);
    assert_eq!(constraints.allow_upstream_proxy, Some(false));
    assert_eq!(constraints.dangerously_allow_non_loopback_proxy, Some(true));
    assert_eq!(constraints.dangerously_allow_all_unix_sockets, Some(true));
    assert_eq!(
        constraints.allowed_domains,
        Some(vec!["example.com".to_string()])
    );
    assert_eq!(
        constraints.denied_domains,
        Some(vec!["blocked.example.com".to_string()])
    );
    assert_eq!(
        constraints.allow_unix_sockets,
        Some(vec!["/run/docker.sock".to_string()])
    );
    assert_eq!(constraints.allow_local_binding, Some(true));
}

#[test]
fn missing_requirements_leave_proxy_constraints_unset() {
    assert_eq!(
        constraints_from_requirements(None),
        NetworkProxyConstraints::default()
    );
}
