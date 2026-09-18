use super::*;
use crate::reasons::REASON_NOT_ALLOWED;
use pretty_assertions::assert_eq;

#[test]
fn blocked_message_with_policy_returns_human_message() {
    let details = PolicyDecisionDetails {
        decision: NetworkPolicyDecision::Ask,
        reason: REASON_NOT_ALLOWED,
        source: NetworkDecisionSource::Decider,
        protocol: NetworkProtocol::HttpsConnect,
        host: "api.example.com",
        port: 443,
    };

    let message = blocked_message_with_policy(REASON_NOT_ALLOWED, &details);
    assert_eq!(
        message,
        "Chaos blocked this request: domain not in allowlist (this is not a denylist block)."
    );
}
