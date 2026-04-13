mod decision;
mod events;
mod helpers;
mod policy;
mod state;

pub use decision::HostBlockDecision;
pub use decision::HostBlockReason;
pub use events::BlockedRequest;
pub use events::BlockedRequestArgs;
pub use events::BlockedRequestObserver;
pub use helpers::NetworkProxyAuditMetadata;
pub(crate) use helpers::unix_socket_permissions_supported;
pub use state::ConfigReloader;
pub use state::ConfigState;
pub use state::NetworkProxyState;

#[cfg(test)]
pub(crate) use test_helpers::network_proxy_state_for_policy;

#[cfg(test)]
mod test_helpers {
    use super::*;
    use crate::config::NetworkMode;
    use crate::config::NetworkProxyConfig;
    use crate::state::NetworkProxyConstraints;
    use crate::state::build_config_state;
    use anyhow::Result;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    pub(crate) fn network_proxy_state_for_policy(
        mut network: crate::config::NetworkProxySettings,
    ) -> NetworkProxyState {
        network.enabled = true;
        network.mode = NetworkMode::Full;
        let config = NetworkProxyConfig { network };
        let state = build_config_state(config, NetworkProxyConstraints::default()).unwrap();

        NetworkProxyState::with_reloader(state, Arc::new(NoopReloader))
    }

    pub(super) struct NoopReloader;

    impl ConfigReloader for NoopReloader {
        fn source_label(&self) -> String {
            "test config state".to_string()
        }

        fn maybe_reload(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Option<ConfigState>>> + Send + '_>> {
            Box::pin(async { Ok(None) })
        }

        fn reload_now(&self) -> Pin<Box<dyn Future<Output = Result<ConfigState>> + Send + '_>> {
            Box::pin(async { Err(anyhow::anyhow!("force reload is not supported in tests")) })
        }
    }
}

#[cfg(test)]
mod tests {
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
            format!("{err:#}")
                .contains("network.allowed_domains constrained by system requirements"),
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
            format!("{err:#}")
                .contains("network.denied_domains constrained by system requirements"),
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
    async fn host_blocked_rejects_loopback_when_local_binding_disabled() {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec!["example.com".to_string()],
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });

        assert_eq!(
            state.host_blocked("127.0.0.1", 80).await.unwrap(),
            HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal)
        );
        assert_eq!(
            state.host_blocked("localhost", 80).await.unwrap(),
            HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal)
        );
    }

    #[tokio::test]
    async fn host_blocked_allows_loopback_when_explicitly_allowlisted_and_local_binding_disabled() {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec!["localhost".to_string()],
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });

        assert_eq!(
            state.host_blocked("localhost", 80).await.unwrap(),
            HostBlockDecision::Allowed
        );
    }

    #[tokio::test]
    async fn host_blocked_allows_private_ip_literal_when_explicitly_allowlisted() {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec!["10.0.0.1".to_string()],
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });

        assert_eq!(
            state.host_blocked("10.0.0.1", 80).await.unwrap(),
            HostBlockDecision::Allowed
        );
    }

    #[tokio::test]
    async fn host_blocked_rejects_scoped_ipv6_literal_when_not_allowlisted() {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec!["example.com".to_string()],
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });

        assert_eq!(
            state.host_blocked("fe80::1%lo0", 80).await.unwrap(),
            HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal)
        );
    }

    #[tokio::test]
    async fn host_blocked_allows_scoped_ipv6_literal_when_explicitly_allowlisted() {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec!["fe80::1%lo0".to_string()],
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });

        assert_eq!(
            state.host_blocked("fe80::1%lo0", 80).await.unwrap(),
            HostBlockDecision::Allowed
        );
    }

    #[tokio::test]
    async fn host_blocked_rejects_private_ip_literals_when_local_binding_disabled() {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec!["example.com".to_string()],
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });

        assert_eq!(
            state.host_blocked("10.0.0.1", 80).await.unwrap(),
            HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal)
        );
    }

    #[tokio::test]
    async fn host_blocked_rejects_loopback_when_allowlist_empty() {
        let state = network_proxy_state_for_policy(NetworkProxySettings {
            allowed_domains: vec![],
            allow_local_binding: false,
            ..NetworkProxySettings::default()
        });

        assert_eq!(
            state.host_blocked("127.0.0.1", 80).await.unwrap(),
            HostBlockDecision::Blocked(HostBlockReason::NotAllowedLocal)
        );
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
    fn validate_policy_against_constraints_allows_narrowing_wildcard_allowlist() {
        let constraints = NetworkProxyConstraints {
            allowed_domains: Some(vec!["*.example.com".to_string()]),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["api.example.com".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_ok());
    }

    #[test]
    fn validate_policy_against_constraints_rejects_widening_wildcard_allowlist() {
        let constraints = NetworkProxyConstraints {
            allowed_domains: Some(vec!["*.example.com".to_string()]),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["**.example.com".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_err());
    }

    #[test]
    fn validate_policy_against_constraints_rejects_global_wildcard_in_managed_allowlist() {
        let constraints = NetworkProxyConstraints {
            allowed_domains: Some(vec!["*".to_string()]),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["api.example.com".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_err());
    }

    #[test]
    fn validate_policy_against_constraints_rejects_bracketed_global_wildcard_in_managed_allowlist()
    {
        let constraints = NetworkProxyConstraints {
            allowed_domains: Some(vec!["[*]".to_string()]),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["api.example.com".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_err());
    }

    #[test]
    fn validate_policy_against_constraints_rejects_double_wildcard_bracketed_global_wildcard_in_managed_allowlist()
     {
        let constraints = NetworkProxyConstraints {
            allowed_domains: Some(vec!["**.[*]".to_string()]),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["api.example.com".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_err());
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
    fn validate_policy_against_constraints_disallows_allow_all_unix_sockets_without_managed_opt_in()
    {
        let constraints = NetworkProxyConstraints {
            dangerously_allow_all_unix_sockets: Some(false),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                dangerously_allow_all_unix_sockets: true,
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_err());
    }

    #[test]
    fn validate_policy_against_constraints_disallows_allow_all_unix_sockets_when_allowlist_is_managed()
     {
        let constraints = NetworkProxyConstraints {
            allow_unix_sockets: Some(vec!["/tmp/allowed.sock".to_string()]),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                dangerously_allow_all_unix_sockets: true,
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_err());
    }

    #[test]
    fn validate_policy_against_constraints_allows_allow_all_unix_sockets_with_managed_opt_in() {
        let constraints = NetworkProxyConstraints {
            dangerously_allow_all_unix_sockets: Some(true),
            ..NetworkProxyConstraints::default()
        };

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                dangerously_allow_all_unix_sockets: true,
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_ok());
    }

    #[test]
    fn validate_policy_against_constraints_allows_allow_all_unix_sockets_when_unmanaged() {
        let constraints = NetworkProxyConstraints::default();

        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                dangerously_allow_all_unix_sockets: true,
                ..NetworkProxySettings::default()
            },
        };

        assert!(validate_policy_against_constraints(&config, &constraints).is_ok());
    }

    #[test]
    fn compile_globset_is_case_insensitive() {
        let patterns = vec!["ExAmPle.CoM".to_string()];
        let set = compile_globset(&patterns).unwrap();
        assert!(set.is_match("example.com"));
        assert!(set.is_match("EXAMPLE.COM"));
    }

    #[test]
    fn compile_globset_excludes_apex_for_subdomain_patterns() {
        let patterns = vec!["*.openai.com".to_string()];
        let set = compile_globset(&patterns).unwrap();
        assert!(set.is_match("api.openai.com"));
        assert!(!set.is_match("openai.com"));
        assert!(!set.is_match("evilopenai.com"));
    }

    #[test]
    fn compile_globset_includes_apex_for_double_wildcard_patterns() {
        let patterns = vec!["**.openai.com".to_string()];
        let set = compile_globset(&patterns).unwrap();
        assert!(set.is_match("openai.com"));
        assert!(set.is_match("api.openai.com"));
        assert!(!set.is_match("evilopenai.com"));
    }

    #[test]
    fn compile_globset_rejects_global_wildcard() {
        let patterns = vec!["*".to_string()];
        assert!(compile_globset(&patterns).is_err());
    }

    #[test]
    fn compile_globset_rejects_bracketed_global_wildcard() {
        let patterns = vec!["[*]".to_string()];
        assert!(compile_globset(&patterns).is_err());
    }

    #[test]
    fn compile_globset_rejects_double_wildcard_bracketed_global_wildcard() {
        let patterns = vec!["**.[*]".to_string()];
        assert!(compile_globset(&patterns).is_err());
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
    fn compile_globset_rejects_invalid_patterns() {
        let patterns = vec!["[".to_string()];
        assert!(compile_globset(&patterns).is_err());
    }

    #[test]
    fn build_config_state_rejects_global_wildcard_allowed_domains() {
        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["*".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(build_config_state(config, NetworkProxyConstraints::default()).is_err());
    }

    #[test]
    fn build_config_state_rejects_bracketed_global_wildcard_allowed_domains() {
        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["[*]".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(build_config_state(config, NetworkProxyConstraints::default()).is_err());
    }

    #[test]
    fn build_config_state_rejects_global_wildcard_denied_domains() {
        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["example.com".to_string()],
                denied_domains: vec!["*".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(build_config_state(config, NetworkProxyConstraints::default()).is_err());
    }

    #[test]
    fn build_config_state_rejects_bracketed_global_wildcard_denied_domains() {
        let config = NetworkProxyConfig {
            network: NetworkProxySettings {
                enabled: true,
                allowed_domains: vec!["example.com".to_string()],
                denied_domains: vec!["[*]".to_string()],
                ..NetworkProxySettings::default()
            },
        };

        assert!(build_config_state(config, NetworkProxyConstraints::default()).is_err());
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
}
