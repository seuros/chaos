use super::*;
use chaos_ipc::approvals::NetworkPolicyAmendment;
use chaos_ipc::approvals::NetworkPolicyRuleAction;
use chaos_pf::BlockedRequest;
use pretty_assertions::assert_eq;

#[test]
fn network_approval_context_requires_ask_from_decider() {
    let payload = NetworkPolicyDecisionPayload {
        decision: NetworkPolicyDecision::Deny,
        source: NetworkDecisionSource::Decider,
        protocol: Some(NetworkApprovalProtocol::Https),
        host: Some("example.com".to_string()),
        reason: Some("not_allowed".to_string()),
        port: Some(443),
    };

    assert_eq!(network_approval_context_from_payload(&payload), None);
}

#[test]
fn network_approval_context_maps_http_https_and_socks_protocols() {
    for (protocol, port) in [
        (NetworkApprovalProtocol::Http, 80),
        (NetworkApprovalProtocol::Https, 443),
        (NetworkApprovalProtocol::Socks5Tcp, 443),
        (NetworkApprovalProtocol::Socks5Udp, 443),
    ] {
        let payload = NetworkPolicyDecisionPayload {
            decision: NetworkPolicyDecision::Ask,
            source: NetworkDecisionSource::Decider,
            protocol: Some(protocol),
            host: Some("example.com".to_string()),
            reason: Some("not_allowed".to_string()),
            port: Some(port),
        };
        assert_eq!(
            network_approval_context_from_payload(&payload),
            Some(NetworkApprovalContext {
                host: "example.com".to_string(),
                protocol,
            })
        );
    }
}

#[test]
fn network_policy_decision_payload_deserializes_proxy_protocol_aliases() {
    for alias in ["https_connect", "http-connect"] {
        let payload: NetworkPolicyDecisionPayload = serde_json::from_value(serde_json::json!({
            "decision": "ask",
            "source": "decider",
            "protocol": alias,
            "host": "example.com",
            "reason": "not_allowed",
            "port": 443
        }))
        .expect("payload should deserialize");
        assert_eq!(
            payload.protocol,
            Some(NetworkApprovalProtocol::Https),
            "{alias}"
        );
    }
}

#[test]
fn execpolicy_network_rule_amendment_maps_protocol_action_and_justification() {
    let amendment = NetworkPolicyAmendment {
        action: NetworkPolicyRuleAction::Deny,
        host: "example.com".to_string(),
    };
    let context = NetworkApprovalContext {
        host: "example.com".to_string(),
        protocol: NetworkApprovalProtocol::Socks5Udp,
    };

    assert_eq!(
        execpolicy_network_rule_amendment(&amendment, &context, "example.com"),
        ExecPolicyNetworkRuleAmendment {
            protocol: ExecPolicyNetworkRuleProtocol::Socks5Udp,
            decision: ExecPolicyDecision::Forbidden,
            justification: "Deny socks5_udp access to example.com".to_string(),
        }
    );
}

#[test]
fn denied_network_policy_message_requires_deny_decision() {
    let blocked = BlockedRequest {
        host: "example.com".to_string(),
        reason: "not_allowed".to_string(),
        client: None,
        method: Some("GET".to_string()),
        mode: None,
        protocol: "http".to_string(),
        decision: Some("ask".to_string()),
        source: Some("decider".to_string()),
        port: Some(80),
        timestamp: 0,
    };
    assert_eq!(denied_network_policy_message(&blocked), None);
}

#[test]
fn denied_network_policy_message_for_denylist_block_is_explicit() {
    let blocked = BlockedRequest {
        host: "example.com".to_string(),
        reason: "denied".to_string(),
        client: None,
        method: Some("GET".to_string()),
        mode: None,
        protocol: "http".to_string(),
        decision: Some("deny".to_string()),
        source: Some("baseline_policy".to_string()),
        port: Some(80),
        timestamp: 0,
    };
    assert_eq!(
            denied_network_policy_message(&blocked),
            Some(
                "Network access to \"example.com\" was blocked: domain is explicitly denied by policy and cannot be approved from this prompt.".to_string()
            )
        );
}
