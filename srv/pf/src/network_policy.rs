use crate::reasons::REASON_POLICY_DENIED;
use crate::runtime::HostBlockDecision;
use crate::runtime::HostBlockReason;
use crate::state::NetworkProxyState;
use anyhow::Result;
use jiff::Timestamp;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

const AUDIT_TARGET: &str = "chaos_snitch.pf";
const POLICY_DECISION_EVENT_NAME: &str = "chaos.pf.policy_decision";
const POLICY_SCOPE_DOMAIN: &str = "domain";
const POLICY_SCOPE_NON_DOMAIN: &str = "non_domain";
const POLICY_DECISION_ALLOW: &str = "allow";
const POLICY_DECISION_DENY: &str = "deny";
const POLICY_REASON_ALLOW: &str = "allow";
const DEFAULT_METHOD: &str = "none";
const DEFAULT_CLIENT_ADDRESS: &str = "unknown";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkProtocol {
    Http,
    HttpsConnect,
    Socks5Tcp,
    Socks5Udp,
}

impl NetworkProtocol {
    pub const fn as_policy_protocol(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::HttpsConnect => "https_connect",
            Self::Socks5Tcp => "socks5_tcp",
            Self::Socks5Udp => "socks5_udp",
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkPolicyDecision {
    Deny,
    Ask,
}

impl NetworkPolicyDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkDecisionSource {
    BaselinePolicy,
    ModeGuard,
    ProxyState,
    Decider,
}

impl NetworkDecisionSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BaselinePolicy => "baseline_policy",
            Self::ModeGuard => "mode_guard",
            Self::ProxyState => "proxy_state",
            Self::Decider => "decider",
        }
    }
}

#[derive(Clone, Debug)]
pub struct NetworkPolicyRequest {
    pub protocol: NetworkProtocol,
    pub host: String,
    pub port: u16,
    pub client_addr: Option<String>,
    pub method: Option<String>,
    pub command: Option<String>,
    pub exec_policy_hint: Option<String>,
}

pub struct NetworkPolicyRequestArgs {
    pub protocol: NetworkProtocol,
    pub host: String,
    pub port: u16,
    pub client_addr: Option<String>,
    pub method: Option<String>,
    pub command: Option<String>,
    pub exec_policy_hint: Option<String>,
}

impl NetworkPolicyRequest {
    pub fn new(args: NetworkPolicyRequestArgs) -> Self {
        let NetworkPolicyRequestArgs {
            protocol,
            host,
            port,
            client_addr,
            method,
            command,
            exec_policy_hint,
        } = args;
        Self {
            protocol,
            host,
            port,
            client_addr,
            method,
            command,
            exec_policy_hint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkDecision {
    Allow,
    Deny {
        reason: String,
        source: NetworkDecisionSource,
        decision: NetworkPolicyDecision,
    },
}

impl NetworkDecision {
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::deny_with_source(reason, NetworkDecisionSource::Decider)
    }

    pub fn ask(reason: impl Into<String>) -> Self {
        Self::ask_with_source(reason, NetworkDecisionSource::Decider)
    }

    pub fn deny_with_source(reason: impl Into<String>, source: NetworkDecisionSource) -> Self {
        let reason = reason.into();
        let reason = if reason.is_empty() {
            REASON_POLICY_DENIED.to_string()
        } else {
            reason
        };
        Self::Deny {
            reason,
            source,
            decision: NetworkPolicyDecision::Deny,
        }
    }

    pub fn ask_with_source(reason: impl Into<String>, source: NetworkDecisionSource) -> Self {
        let reason = reason.into();
        let reason = if reason.is_empty() {
            REASON_POLICY_DENIED.to_string()
        } else {
            reason
        };
        Self::Deny {
            reason,
            source,
            decision: NetworkPolicyDecision::Ask,
        }
    }
}

pub(crate) struct BlockDecisionAuditEventArgs<'a> {
    pub source: NetworkDecisionSource,
    pub reason: &'a str,
    pub protocol: NetworkProtocol,
    pub server_address: &'a str,
    pub server_port: u16,
    pub method: Option<&'a str>,
    pub client_addr: Option<&'a str>,
}

pub(crate) fn emit_block_decision_audit_event(
    state: &NetworkProxyState,
    args: BlockDecisionAuditEventArgs<'_>,
) {
    emit_non_domain_policy_decision_audit_event(state, args, POLICY_DECISION_DENY);
}

pub(crate) fn emit_allow_decision_audit_event(
    state: &NetworkProxyState,
    args: BlockDecisionAuditEventArgs<'_>,
) {
    emit_non_domain_policy_decision_audit_event(state, args, POLICY_DECISION_ALLOW);
}

fn emit_non_domain_policy_decision_audit_event(
    state: &NetworkProxyState,
    args: BlockDecisionAuditEventArgs<'_>,
    decision: &'static str,
) {
    emit_policy_audit_event(
        state,
        PolicyAuditEventArgs {
            scope: POLICY_SCOPE_NON_DOMAIN,
            decision,
            source: args.source.as_str(),
            reason: args.reason,
            protocol: args.protocol,
            server_address: args.server_address,
            server_port: args.server_port,
            method: args.method,
            client_addr: args.client_addr,
            policy_override: false,
        },
    );
}

struct PolicyAuditEventArgs<'a> {
    scope: &'static str,
    decision: &'a str,
    source: &'a str,
    reason: &'a str,
    protocol: NetworkProtocol,
    server_address: &'a str,
    server_port: u16,
    method: Option<&'a str>,
    client_addr: Option<&'a str>,
    policy_override: bool,
}

fn emit_policy_audit_event(state: &NetworkProxyState, args: PolicyAuditEventArgs<'_>) {
    let metadata = state.audit_metadata();
    tracing::event!(
        target: AUDIT_TARGET,
        tracing::Level::INFO,
        event.name = POLICY_DECISION_EVENT_NAME,
        event.timestamp = %audit_timestamp(),
        conversation.id = metadata.conversation_id.as_deref(),
        app.version = metadata.app_version.as_deref(),
        auth_mode = metadata.auth_mode.as_deref(),
        originator = metadata.originator.as_deref(),
        terminal.type = metadata.terminal_type.as_deref(),
        model = metadata.model.as_deref(),
        slug = metadata.slug.as_deref(),
        network.policy.scope = args.scope,
        network.policy.decision = args.decision,
        network.policy.source = args.source,
        network.policy.reason = args.reason,
        network.transport.protocol = args.protocol.as_policy_protocol(),
        server.address = args.server_address,
        server.port = args.server_port,
        http.request.method = args.method.unwrap_or(DEFAULT_METHOD),
        client.address = args.client_addr.unwrap_or(DEFAULT_CLIENT_ADDRESS),
        network.policy.override = args.policy_override,
    );
}

fn audit_timestamp() -> String {
    Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Decide whether a network request should be allowed.
///
/// If `command` or `exec_policy_hint` is provided, callers can map exec-policy
/// approvals to network access (e.g., allow all requests for commands matching
/// approved prefixes like `curl *`).
pub trait NetworkPolicyDecider: Send + Sync + 'static {
    fn decide(
        &self,
        req: NetworkPolicyRequest,
    ) -> Pin<Box<dyn Future<Output = NetworkDecision> + Send + '_>>;
}

impl<D: NetworkPolicyDecider + ?Sized> NetworkPolicyDecider for Arc<D> {
    fn decide(
        &self,
        req: NetworkPolicyRequest,
    ) -> Pin<Box<dyn Future<Output = NetworkDecision> + Send + '_>> {
        Box::pin(async move { (**self).decide(req).await })
    }
}

impl<F, Fut> NetworkPolicyDecider for F
where
    F: Fn(NetworkPolicyRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = NetworkDecision> + Send,
{
    fn decide(
        &self,
        req: NetworkPolicyRequest,
    ) -> Pin<Box<dyn Future<Output = NetworkDecision> + Send + '_>> {
        Box::pin(async move { (self)(req).await })
    }
}

pub(crate) async fn evaluate_host_policy(
    state: &NetworkProxyState,
    decider: Option<&Arc<dyn NetworkPolicyDecider>>,
    request: &NetworkPolicyRequest,
) -> Result<NetworkDecision> {
    let host_decision = state.host_blocked(&request.host, request.port).await?;
    let (decision, policy_override) = match host_decision {
        HostBlockDecision::Allowed => (NetworkDecision::Allow, false),
        HostBlockDecision::Blocked(HostBlockReason::NotAllowed) => {
            if let Some(decider) = decider {
                let decider_decision = map_decider_decision(decider.decide(request.clone()).await);
                let policy_override = matches!(decider_decision, NetworkDecision::Allow);
                (decider_decision, policy_override)
            } else {
                (
                    NetworkDecision::deny_with_source(
                        HostBlockReason::NotAllowed.as_str(),
                        NetworkDecisionSource::BaselinePolicy,
                    ),
                    false,
                )
            }
        }
        HostBlockDecision::Blocked(reason) => (
            NetworkDecision::deny_with_source(
                reason.as_str(),
                NetworkDecisionSource::BaselinePolicy,
            ),
            false,
        ),
    };

    let (policy_decision, source, reason) = match &decision {
        NetworkDecision::Allow => (
            POLICY_DECISION_ALLOW,
            if policy_override {
                NetworkDecisionSource::Decider
            } else {
                NetworkDecisionSource::BaselinePolicy
            },
            if policy_override {
                HostBlockReason::NotAllowed.as_str()
            } else {
                POLICY_REASON_ALLOW
            },
        ),
        NetworkDecision::Deny {
            reason,
            source,
            decision,
        } => (decision.as_str(), *source, reason.as_str()),
    };

    emit_policy_audit_event(
        state,
        PolicyAuditEventArgs {
            scope: POLICY_SCOPE_DOMAIN,
            decision: policy_decision,
            source: source.as_str(),
            reason,
            protocol: request.protocol,
            server_address: request.host.as_str(),
            server_port: request.port,
            method: request.method.as_deref(),
            client_addr: request.client_addr.as_deref(),
            policy_override,
        },
    );

    Ok(decision)
}

fn map_decider_decision(decision: NetworkDecision) -> NetworkDecision {
    match decision {
        NetworkDecision::Allow => NetworkDecision::Allow,
        NetworkDecision::Deny {
            reason, decision, ..
        } => NetworkDecision::Deny {
            reason,
            source: NetworkDecisionSource::Decider,
            decision,
        },
    }
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
