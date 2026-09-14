use super::test_support::capture_events;
use super::test_support::find_event_by_name;
use super::*;
use crate::config::NetworkMode;
use crate::config::NetworkProxyConfig;
use crate::config::NetworkProxySettings;
use crate::reasons::REASON_DENIED;
use crate::reasons::REASON_METHOD_NOT_ALLOWED;
use crate::reasons::REASON_NOT_ALLOWED;
use crate::reasons::REASON_NOT_ALLOWED_LOCAL;
use crate::runtime::ConfigReloader;
use crate::runtime::ConfigState;
use crate::runtime::NetworkProxyAuditMetadata;
use crate::state::NetworkProxyConstraints;
use crate::state::build_config_state;
use crate::state::network_proxy_state_for_policy;
use chaos_test_fixtures::TEST_MODEL;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

const LEGACY_DOMAIN_POLICY_DECISION_EVENT_NAME: &str = "chaos.network_proxy.domain_policy_decision";
const LEGACY_BLOCK_DECISION_EVENT_NAME: &str = "chaos.network_proxy.block_decision";

#[derive(Clone)]
struct StaticReloader {
    state: ConfigState,
}

impl ConfigReloader for StaticReloader {
    fn maybe_reload(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<ConfigState>>> + Send + '_>> {
        Box::pin(async { Ok(None) })
    }

    fn reload_now(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<ConfigState>> + Send + '_>> {
        let state = self.state.clone();
        Box::pin(async move { Ok(state) })
    }

    fn source_label(&self) -> String {
        "static test reloader".to_string()
    }
}

fn state_with_metadata(metadata: NetworkProxyAuditMetadata) -> NetworkProxyState {
    let network = NetworkProxySettings {
        enabled: true,
        mode: NetworkMode::Full,
        ..NetworkProxySettings::default()
    };
    let config = NetworkProxyConfig { network };
    let state = build_config_state(config, NetworkProxyConstraints::default()).unwrap();
    let reloader = Arc::new(StaticReloader {
        state: state.clone(),
    });
    NetworkProxyState::with_reloader_and_audit_metadata(state, reloader, metadata)
}

fn is_rfc3339_utc_millis(timestamp: &str) -> bool {
    let bytes = timestamp.as_bytes();
    if bytes.len() != 24 {
        return false;
    }
    bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'.'
        && bytes[23] == b'Z'
        && bytes.iter().enumerate().all(|(idx, value)| match idx {
            4 | 7 | 10 | 13 | 16 | 19 | 23 => true,
            _ => value.is_ascii_digit(),
        })
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_host_policy_emits_domain_event_for_decider_allow_override() {
    let state = network_proxy_state_for_policy(NetworkProxySettings::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let decider: Arc<dyn NetworkPolicyDecider> = Arc::new({
        let calls = calls.clone();
        move |_req| {
            calls.fetch_add(1, Ordering::SeqCst);
            // The default policy denies all; the decider is consulted for not_allowed
            // requests and can override that decision.
            async { NetworkDecision::Allow }
        }
    });

    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Http,
        host: "example.com".to_string(),
        port: 80,
        client_addr: None,
        method: None,
        command: None,
        exec_policy_hint: None,
    });

    let (decision, events) = capture_events(|| async {
        evaluate_host_policy(&state, Some(&decider), &request)
            .await
            .unwrap()
    })
    .await;
    assert_eq!(decision, NetworkDecision::Allow);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let event = find_event_by_name(&events, POLICY_DECISION_EVENT_NAME)
        .expect("expected policy decision audit event");
    assert_eq!(event.target, AUDIT_TARGET);
    assert!(event.target.starts_with("chaos_snitch."));
    assert_eq!(
        event.field("network.policy.scope"),
        Some(POLICY_SCOPE_DOMAIN)
    );
    assert_eq!(event.field("network.policy.decision"), Some("allow"));
    assert_eq!(event.field("network.policy.source"), Some("decider"));
    assert_eq!(
        event.field("network.policy.reason"),
        Some(REASON_NOT_ALLOWED)
    );
    assert_eq!(event.field("network.transport.protocol"), Some("http"));
    assert_eq!(event.field("server.address"), Some("example.com"));
    assert_eq!(event.field("server.port"), Some("80"));
    assert_eq!(event.field("http.request.method"), Some(DEFAULT_METHOD));
    assert_eq!(event.field("client.address"), Some(DEFAULT_CLIENT_ADDRESS));
    assert_eq!(event.field("network.policy.override"), Some("true"));
    let timestamp = event
        .field("event.timestamp")
        .expect("event timestamp should be present");
    assert!(is_rfc3339_utc_millis(timestamp));
    assert_eq!(
        find_event_by_name(&events, LEGACY_DOMAIN_POLICY_DECISION_EVENT_NAME),
        None
    );
    assert_eq!(
        find_event_by_name(&events, LEGACY_BLOCK_DECISION_EVENT_NAME),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_host_policy_emits_domain_event_for_baseline_deny() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        denied_domains: vec!["blocked.com".to_string()],
        ..NetworkProxySettings::default()
    });
    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Http,
        host: "blocked.com".to_string(),
        port: 80,
        client_addr: Some("127.0.0.1:1234".to_string()),
        method: Some("GET".to_string()),
        command: None,
        exec_policy_hint: None,
    });

    let (decision, events) =
        capture_events(|| async { evaluate_host_policy(&state, None, &request).await.unwrap() })
            .await;
    assert_eq!(
        decision,
        NetworkDecision::Deny {
            reason: REASON_DENIED.to_string(),
            source: NetworkDecisionSource::BaselinePolicy,
            decision: NetworkPolicyDecision::Deny,
        }
    );

    let event = find_event_by_name(&events, POLICY_DECISION_EVENT_NAME)
        .expect("expected policy decision audit event");
    assert_eq!(event.field("network.policy.decision"), Some("deny"));
    assert_eq!(
        event.field("network.policy.source"),
        Some("baseline_policy")
    );
    assert_eq!(event.field("network.policy.reason"), Some(REASON_DENIED));
    assert_eq!(event.field("network.policy.override"), Some("false"));
    assert_eq!(event.field("http.request.method"), Some("GET"));
    assert_eq!(event.field("client.address"), Some("127.0.0.1:1234"));
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_host_policy_emits_domain_event_for_decider_ask() {
    let state = network_proxy_state_for_policy(NetworkProxySettings::default());
    let decider: Arc<dyn NetworkPolicyDecider> =
        Arc::new(|_req| async { NetworkDecision::ask(REASON_NOT_ALLOWED) });
    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Http,
        host: "example.com".to_string(),
        port: 80,
        client_addr: None,
        method: Some("GET".to_string()),
        command: None,
        exec_policy_hint: None,
    });

    let (decision, events) = capture_events(|| async {
        evaluate_host_policy(&state, Some(&decider), &request)
            .await
            .unwrap()
    })
    .await;
    assert_eq!(
        decision,
        NetworkDecision::Deny {
            reason: REASON_NOT_ALLOWED.to_string(),
            source: NetworkDecisionSource::Decider,
            decision: NetworkPolicyDecision::Ask,
        }
    );

    let event = find_event_by_name(&events, POLICY_DECISION_EVENT_NAME)
        .expect("expected policy decision audit event");
    assert_eq!(event.field("network.policy.decision"), Some("ask"));
    assert_eq!(event.field("network.policy.source"), Some("decider"));
    assert_eq!(
        event.field("network.policy.reason"),
        Some(REASON_NOT_ALLOWED)
    );
    assert_eq!(event.field("network.policy.override"), Some("false"));
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_host_policy_emits_metadata_fields() {
    let metadata = NetworkProxyAuditMetadata {
        conversation_id: Some("conversation-1".to_string()),
        app_version: Some("1.2.3".to_string()),
        auth_mode: Some("Chatgpt".to_string()),
        originator: Some("free_chaos".to_string()),
        terminal_type: Some("iTerm.app/3.6.5".to_string()),
        model: Some(TEST_MODEL.to_string()),
        slug: Some(TEST_MODEL.to_string()),
    };
    let state = state_with_metadata(metadata);
    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Http,
        host: "example.com".to_string(),
        port: 80,
        client_addr: None,
        method: Some("GET".to_string()),
        command: None,
        exec_policy_hint: None,
    });

    let (_decision, events) =
        capture_events(|| async { evaluate_host_policy(&state, None, &request).await.unwrap() })
            .await;

    let event = find_event_by_name(&events, POLICY_DECISION_EVENT_NAME)
        .expect("expected policy decision audit event");
    assert_eq!(event.field("conversation.id"), Some("conversation-1"));
    assert_eq!(event.field("app.version"), Some("1.2.3"));
    assert_eq!(event.field("auth_mode"), Some("Chatgpt"));
    assert_eq!(event.field("originator"), Some("free_chaos"));
    assert_eq!(event.field("terminal.type"), Some("iTerm.app/3.6.5"));
    assert_eq!(event.field("model"), Some(TEST_MODEL));
    assert_eq!(event.field("slug"), Some(TEST_MODEL));
}

#[tokio::test(flavor = "current_thread")]
async fn emit_block_decision_audit_event_emits_non_domain_event() {
    let state = network_proxy_state_for_policy(NetworkProxySettings::default());

    let (_, events) = capture_events(|| async {
        emit_block_decision_audit_event(
            &state,
            BlockDecisionAuditEventArgs {
                source: NetworkDecisionSource::ModeGuard,
                reason: REASON_METHOD_NOT_ALLOWED,
                protocol: NetworkProtocol::Http,
                server_address: "unix-socket",
                server_port: 0,
                method: Some("POST"),
                client_addr: None,
            },
        );
    })
    .await;

    let event = find_event_by_name(&events, POLICY_DECISION_EVENT_NAME)
        .expect("expected policy decision audit event");
    assert_eq!(event.target, AUDIT_TARGET);
    assert_eq!(
        event.field("network.policy.scope"),
        Some(POLICY_SCOPE_NON_DOMAIN)
    );
    assert_eq!(
        event.field("network.policy.decision"),
        Some(POLICY_DECISION_DENY)
    );
    assert_eq!(event.field("network.policy.source"), Some("mode_guard"));
    assert_eq!(
        event.field("network.policy.reason"),
        Some(REASON_METHOD_NOT_ALLOWED)
    );
    assert_eq!(event.field("network.transport.protocol"), Some("http"));
    assert_eq!(event.field("server.address"), Some("unix-socket"));
    assert_eq!(event.field("server.port"), Some("0"));
    assert_eq!(event.field("http.request.method"), Some("POST"));
    assert_eq!(event.field("client.address"), Some(DEFAULT_CLIENT_ADDRESS));
    assert_eq!(event.field("network.policy.override"), Some("false"));
    assert_eq!(
        find_event_by_name(&events, LEGACY_BLOCK_DECISION_EVENT_NAME),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_host_policy_still_denies_not_allowed_local_without_decider_override() {
    let state = network_proxy_state_for_policy(NetworkProxySettings {
        allowed_domains: vec!["example.com".to_string()],
        allow_local_binding: false,
        ..NetworkProxySettings::default()
    });
    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Http,
        host: "127.0.0.1".to_string(),
        port: 80,
        client_addr: None,
        method: Some("GET".to_string()),
        command: None,
        exec_policy_hint: None,
    });

    let decision = evaluate_host_policy(&state, None, &request).await.unwrap();
    assert_eq!(
        decision,
        NetworkDecision::Deny {
            reason: REASON_NOT_ALLOWED_LOCAL.to_string(),
            source: NetworkDecisionSource::BaselinePolicy,
            decision: NetworkPolicyDecision::Deny,
        }
    );
}

#[test]
fn ask_uses_decider_source_and_ask_decision() {
    assert_eq!(
        NetworkDecision::ask(REASON_NOT_ALLOWED),
        NetworkDecision::Deny {
            reason: REASON_NOT_ALLOWED.to_string(),
            source: NetworkDecisionSource::Decider,
            decision: NetworkPolicyDecision::Ask,
        }
    );
}
