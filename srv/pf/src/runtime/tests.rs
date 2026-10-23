use super::*;

use crate::config::NetworkMode;
use crate::config::NetworkProxyConfig;
use crate::config::NetworkProxySettings;
use crate::policy::compile_globset;
use crate::state::NetworkProxyConstraints;
use crate::state::build_config_state;
use crate::state::validate_policy_against_constraints;
use events::BlockedRequestArgs;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use test_helpers::network_proxy_state_for_policy;

#[tokio::test]
async fn host_blocked_denied_wins_over_allowed() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        denied_domains: vec!["example.com".to_string()],
        ..NetworkProxySettings::default()
    });

    assert_eq!(
        state.host_blocked("example.com", 80).await.unwrap(),
        HostBlockDecision::Blocked(HostBlockReason::Denied)
    );
}

#[tokio::test]
async fn host_blocked_requires_allowlist_match() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        ..NetworkProxySettings::default()
    });

    assert_eq!(
        state.host_blocked("example.com", 80).await.unwrap(),
        HostBlockDecision::Allowed
    );
    assert_eq!(
        // Use a public IP literal to avoid relying on ambient DNS behavior (some networks
        // resolve unknown hostnames to private IPs, which would trigger `not_allowed_local`).
        state.host_blocked("8.8.8.8", 80).await.unwrap(),
        HostBlockDecision::Blocked(HostBlockReason::NotAllowed)
    );
}

#[tokio::test]
async fn add_allowed_domain_removes_matching_deny_entry() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        denied_domains: vec!["example.com".to_string()],
        ..NetworkProxySettings::default()
    });

    state.add_allowed_domain("ExAmPlE.CoM").await.unwrap();

    let (allowed, denied) = state.current_patterns().await.unwrap();
    assert_eq!(allowed, vec!["example.com".to_string()]);
    assert!(denied.is_empty());
    assert_eq!(
        state.host_blocked("example.com", 80).await.unwrap(),
        HostBlockDecision::Allowed
    );
}

#[tokio::test]
async fn add_denied_domain_removes_matching_allow_entry() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        ..NetworkProxySettings::default()
    });

    state.add_denied_domain("EXAMPLE.COM").await.unwrap();

    let (allowed, denied) = state.current_patterns().await.unwrap();
    assert!(allowed.is_empty());
    assert_eq!(denied, vec!["example.com".to_string()]);
    assert_eq!(
        state.host_blocked("example.com", 80).await.unwrap(),
        HostBlockDecision::Blocked(HostBlockReason::Denied)
    );
}

#[tokio::test]
async fn add_allowed_domain_succeeds_when_managed_baseline_allows_expansion() {
    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            allowed_domains: vec!["managed.example.com".to_string()],
            ..NetworkProxySettings::default()
        },
    };
    let constraints = NetworkProxyConstraints {
        allowed_domains: Some(vec!["managed.example.com".to_string()]),
        allowlist_expansion_enabled: Some(true),
        ..NetworkProxyConstraints::default()
    };
    let state = NetworkProxyState::with_reloader(
        build_config_state(config, constraints).unwrap(),
        Arc::new(test_helpers::NoopReloader),
    );

    state.add_allowed_domain("user.example.com").await.unwrap();

    let (allowed, denied) = state.current_patterns().await.unwrap();
    assert_eq!(
        allowed,
        vec![
            "managed.example.com".to_string(),
            "user.example.com".to_string()
        ]
    );
    assert!(denied.is_empty());
}

#[tokio::test]
async fn add_allowed_domain_rejects_expansion_when_managed_baseline_is_fixed() {
    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            allowed_domains: vec!["managed.example.com".to_string()],
            ..NetworkProxySettings::default()
        },
    };
    let constraints = NetworkProxyConstraints {
        allowed_domains: Some(vec!["managed.example.com".to_string()]),
        allowlist_expansion_enabled: Some(false),
        ..NetworkProxyConstraints::default()
    };
    let state = NetworkProxyState::with_reloader(
        build_config_state(config, constraints).unwrap(),
        Arc::new(test_helpers::NoopReloader),
    );

    let err = state
        .add_allowed_domain("user.example.com")
        .await
        .expect_err("managed baseline should reject allowlist expansion");

    assert!(
        format!("{err:#}").contains("network.allowed_domains constrained by system requirements"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn add_denied_domain_rejects_expansion_when_managed_baseline_is_fixed() {
    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            denied_domains: vec!["managed.example.com".to_string()],
            ..NetworkProxySettings::default()
        },
    };
    let constraints = NetworkProxyConstraints {
        denied_domains: Some(vec!["managed.example.com".to_string()]),
        denylist_expansion_enabled: Some(false),
        ..NetworkProxyConstraints::default()
    };
    let state = NetworkProxyState::with_reloader(
        build_config_state(config, constraints).unwrap(),
        Arc::new(test_helpers::NoopReloader),
    );

    let err = state
        .add_denied_domain("user.example.com")
        .await
        .expect_err("managed baseline should reject denylist expansion");

    assert!(
        format!("{err:#}").contains("network.denied_domains constrained by system requirements"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn blocked_snapshot_does_not_consume_entries() {
    let state = network_proxy_state_for_policy(NetworkProxySettings::default());

    state
        .record_blocked(BlockedRequest::new(BlockedRequestArgs {
            host: "google.com".to_string(),
            reason: "not_allowed".to_string(),
            client: None,
            method: Some("GET".to_string()),
            mode: None,
            protocol: "http".to_string(),
            decision: Some("ask".to_string()),
            source: Some("decider".to_string()),
            port: Some(80),
        }))
        .await
        .expect("entry should be recorded");

    let snapshot = state
        .blocked_snapshot()
        .await
        .expect("snapshot should succeed");
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].host, "google.com");
    assert_eq!(snapshot[0].decision.as_deref(), Some("ask"));

    let drained = state
        .drain_blocked()
        .await
        .expect("drain should include snapshot entry");
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].host, snapshot[0].host);
    assert_eq!(drained[0].reason, snapshot[0].reason);
    assert_eq!(drained[0].decision, snapshot[0].decision);
    assert_eq!(drained[0].source, snapshot[0].source);
    assert_eq!(drained[0].port, snapshot[0].port);
}

#[tokio::test]
async fn drain_blocked_returns_buffered_window() {
    let state = network_proxy_state_for_policy(NetworkProxySettings::default());

    for idx in 0..(events::MAX_BLOCKED_EVENTS + 5) {
        state
            .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                host: format!("example{idx}.com"),
                reason: "not_allowed".to_string(),
                client: None,
                method: Some("GET".to_string()),
                mode: None,
                protocol: "http".to_string(),
                decision: Some("ask".to_string()),
                source: Some("decider".to_string()),
                port: Some(80),
            }))
            .await
            .expect("entry should be recorded");
    }

    let blocked = state.drain_blocked().await.expect("drain should succeed");
    assert_eq!(blocked.len(), events::MAX_BLOCKED_EVENTS);
    assert_eq!(blocked[0].host, "example5.com");
}

#[test]
fn blocked_request_violation_log_line_serializes_payload() {
    let entry = BlockedRequest {
        host: "google.com".to_string(),
        reason: "not_allowed".to_string(),
        client: Some("127.0.0.1".to_string()),
        method: Some("GET".to_string()),
        mode: Some(NetworkMode::Full),
        protocol: "http".to_string(),
        decision: Some("ask".to_string()),
        source: Some("decider".to_string()),
        port: Some(80),
        timestamp: 1_735_689_600,
    };

    assert_eq!(
        events::blocked_request_violation_log_line(&entry),
        r#"CHAOS_NETWORK_POLICY_VIOLATION {"host":"google.com","reason":"not_allowed","client":"127.0.0.1","method":"GET","mode":"full","protocol":"http","decision":"ask","source":"decider","port":80,"timestamp":1735689600}"#
    );
}

#[tokio::test]
async fn host_blocked_subdomain_wildcards_exclude_apex() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["*.openai.com".to_string()],
        ..NetworkProxySettings::default()
    });

    assert_eq!(
        state.host_blocked("api.openai.com", 80).await.unwrap(),
        HostBlockDecision::Allowed
    );
    assert_eq!(
        state.host_blocked("openai.com", 80).await.unwrap(),
        HostBlockDecision::Blocked(HostBlockReason::NotAllowed)
    );
}

#[tokio::test]
async fn local_targets_require_an_explicit_allowlist_even_when_local_binding_is_disabled() {
    for (allowed, host, is_allowed) in [
        (Some("example.com"), "127.0.0.1", false),
        (Some("example.com"), "localhost", false),
        (Some("localhost"), "localhost", true),
        (Some("10.0.0.1"), "10.0.0.1", true),
        (Some("example.com"), "fe80::1%lo0", false),
        (Some("fe80::1%lo0"), "fe80::1%lo0", true),
        (Some("example.com"), "10.0.0.1", false),
        (None, "127.0.0.1", false),
    ] {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: allowed.into_iter().map(str::to_string).collect(),
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });
        let expected = if is_allowed {
            HostBlockDecision::Allowed
        } else {
            HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal)
        };
        assert_eq!(
            state.host_blocked(host, 80).await.unwrap(),
            expected,
            "allowed={allowed:?}, host={host}"
        );
    }
}

#[test]
fn validate_policy_against_constraints_disallows_widening_allowed_domains() {
    let constraints = NetworkProxyConstraints {
        allowed_domains: Some(vec!["example.com".to_string()]),
        ..NetworkProxyConstraints::default()
    };

    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            allowed_domains: vec!["example.com".to_string(), "evil.com".to_string()],
            ..NetworkProxySettings::default()
        },
    };

    assert!(validate_policy_against_constraints(&config, &constraints).is_err());
}

#[test]
fn validate_policy_against_constraints_allows_expanding_allowed_domains_when_enabled() {
    let constraints = NetworkProxyConstraints {
        allowed_domains: Some(vec!["example.com".to_string()]),
        allowlist_expansion_enabled: Some(true),
        ..NetworkProxyConstraints::default()
    };

    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            allowed_domains: vec!["example.com".to_string(), "api.openai.com".to_string()],
            ..NetworkProxySettings::default()
        },
    };

    assert!(validate_policy_against_constraints(&config, &constraints).is_ok());
}

#[test]
fn validate_policy_against_constraints_disallows_widening_mode() {
    let constraints = NetworkProxyConstraints {
        mode: Some(NetworkMode::Limited),
        ..NetworkProxyConstraints::default()
    };

    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            mode: NetworkMode::Full,
            ..NetworkProxySettings::default()
        },
    };

    assert!(validate_policy_against_constraints(&config, &constraints).is_err());
}

#[test]
fn managed_wildcard_allowlist_permits_narrowing_but_not_apex_expansion() {
    let constraints = NetworkProxyConstraints {
        allowed_domains: Some(vec!["*.example.com".to_string()]),
        ..NetworkProxyConstraints::default()
    };

    for (pattern, permitted) in [("api.example.com", true), ("**.example.com", false)] {
        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec![pattern.to_string()],
                ..NetworkProxySettings::default()
            },
        };
        assert_eq!(
            validate_policy_against_constraints(&config, &constraints).is_ok(),
            permitted,
            "{pattern}"
        );
    }
}

#[test]
fn managed_allowlist_rejects_all_global_wildcard_spellings() {
    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            allowed_domains: vec!["api.example.com".to_string()],
            ..NetworkProxySettings::default()
        },
    };

    for pattern in ["*", "[*]", "**.[*]"] {
        let constraints = NetworkProxyConstraints {
            allowed_domains: Some(vec![pattern.to_string()]),
            ..NetworkProxyConstraints::default()
        };
        assert!(
            validate_policy_against_constraints(&config, &constraints).is_err(),
            "{pattern}"
        );
    }
}

#[test]
fn validate_policy_against_constraints_requires_managed_denied_domains_entries() {
    let constraints = NetworkProxyConstraints {
        denied_domains: Some(vec!["evil.com".to_string()]),
        ..NetworkProxyConstraints::default()
    };

    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            denied_domains: vec![],
            ..NetworkProxySettings::default()
        },
    };

    assert!(validate_policy_against_constraints(&config, &constraints).is_err());
}

#[test]
fn validate_policy_against_constraints_disallows_expanding_denied_domains_when_fixed() {
    let constraints = NetworkProxyConstraints {
        denied_domains: Some(vec!["evil.com".to_string()]),
        denylist_expansion_enabled: Some(false),
        ..NetworkProxyConstraints::default()
    };

    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            denied_domains: vec!["evil.com".to_string(), "more-evil.com".to_string()],
            ..NetworkProxySettings::default()
        },
    };

    assert!(validate_policy_against_constraints(&config, &constraints).is_err());
}

#[test]
fn validate_policy_against_constraints_disallows_enabling_when_managed_disabled() {
    let constraints = NetworkProxyConstraints {
        enabled: Some(false),
        ..NetworkProxyConstraints::default()
    };

    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            ..NetworkProxySettings::default()
        },
    };

    assert!(validate_policy_against_constraints(&config, &constraints).is_err());
}

#[test]
fn validate_policy_against_constraints_disallows_allow_local_binding_when_managed_disabled() {
    let constraints = NetworkProxyConstraints {
        allow_local_binding: Some(false),
        ..NetworkProxyConstraints::default()
    };

    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            allow_local_binding: true,
            ..NetworkProxySettings::default()
        },
    };

    assert!(validate_policy_against_constraints(&config, &constraints).is_err());
}

#[test]
fn all_unix_socket_access_respects_managed_opt_in_and_allowlists() {
    let config = NetworkProxyConfig {
        network: NetworkProxySettings {
            enabled: true,
            dangerously_allow_all_unix_sockets: true,
            ..NetworkProxySettings::default()
        },
    };

    for (label, opt_in, allowlist, permitted) in [
        ("managed opt-out", Some(false), None, false),
        (
            "managed allowlist",
            None,
            Some(vec!["/tmp/allowed.sock".to_string()]),
            false,
        ),
        ("managed opt-in", Some(true), None, true),
        ("unmanaged", None, None, true),
    ] {
        let constraints = NetworkProxyConstraints {
            dangerously_allow_all_unix_sockets: opt_in,
            allow_unix_sockets: allowlist,
            ..NetworkProxyConstraints::default()
        };
        assert_eq!(
            validate_policy_against_constraints(&config, &constraints).is_ok(),
            permitted,
            "{label}"
        );
    }
}

#[test]
fn compile_globset_dedupes_patterns_without_changing_behavior() {
    let patterns = vec!["example.com".to_string(), "example.com".to_string()];
    let set = compile_globset(&patterns).unwrap();
    assert!(set.is_match("example.com"));
    assert!(set.is_match("EXAMPLE.COM"));
    assert!(!set.is_match("not-example.com"));
}

#[test]
fn compile_globset_rejects_global_wildcards_and_invalid_patterns() {
    for pattern in ["*", "[*]", "**.[*]", "["] {
        assert!(
            compile_globset(&[pattern.to_string()]).is_err(),
            "{pattern}"
        );
    }
}

#[test]
fn build_config_state_rejects_global_wildcards_in_both_domain_lists() {
    for pattern in ["*", "[*]"] {
        for denied in [false, true] {
            let config = NetworkProxyConfig {
                network: NetworkProxySettings {
                    enabled: true,
                    allowed_domains: vec![if denied { "example.com" } else { pattern }.to_string()],
                    denied_domains: if denied {
                        vec![pattern.to_string()]
                    } else {
                        vec![]
                    },
                    ..NetworkProxySettings::default()
                },
            };
            assert!(
                build_config_state(config, NetworkProxyConstraints::default()).is_err(),
                "{pattern}, denied={denied}"
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn unix_socket_allowlist_is_respected_on_macos() {
    let socket_path = "/tmp/example.sock".to_string();
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        allow_unix_sockets: vec![socket_path.clone()],
        ..NetworkProxySettings::default()
    });

    assert!(state.is_unix_socket_allowed(&socket_path).await.unwrap());
    assert!(
        !state
            .is_unix_socket_allowed("/tmp/not-allowed.sock")
            .await
            .unwrap()
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn unix_socket_allowlist_resolves_symlinks() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let temp_dir = tempdir().unwrap();
    let dir = temp_dir.path();

    let real = dir.join("real.sock");
    let link = dir.join("link.sock");

    // The allowlist mechanism is path-based; for test purposes we don't need an actual unix
    // domain socket. Any filesystem entry works for canonicalization.
    std::fs::write(&real, b"not a socket").unwrap();
    symlink(&real, &link).unwrap();

    let real_s = real.to_str().unwrap().to_string();
    let link_s = link.to_str().unwrap().to_string();

    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        allow_unix_sockets: vec![real_s],
        ..NetworkProxySettings::default()
    });

    assert!(state.is_unix_socket_allowed(&link_s).await.unwrap());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn unix_socket_allow_all_flag_bypasses_allowlist() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        dangerously_allow_all_unix_sockets: true,
        ..NetworkProxySettings::default()
    });

    assert!(state.is_unix_socket_allowed("/tmp/any.sock").await.unwrap());
    assert!(!state.is_unix_socket_allowed("relative.sock").await.unwrap());
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn unix_socket_allowlist_is_rejected_on_non_macos() {
    let socket_path = "/tmp/example.sock".to_string();
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        allow_unix_sockets: vec![socket_path.clone()],
        dangerously_allow_all_unix_sockets: true,
        ..NetworkProxySettings::default()
    });

    assert!(!state.is_unix_socket_allowed(&socket_path).await.unwrap());
}
