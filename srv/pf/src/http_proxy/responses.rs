use crate::network_policy::BlockDecisionAuditEventArgs;
use crate::network_policy::NetworkPolicyDecision;
use crate::network_policy::NetworkProtocol;
use crate::network_policy::emit_allow_decision_audit_event;
use crate::network_policy::emit_block_decision_audit_event;
use crate::reasons::REASON_PROXY_DISABLED;
use crate::responses::PolicyDecisionDetails;
use crate::responses::blocked_header_value;
use crate::responses::blocked_message_with_policy;
use crate::responses::blocked_text_response_with_policy;
use crate::responses::json_response;
use crate::state::BlockedRequest;
use crate::state::BlockedRequestArgs;
use crate::state::NetworkProxyState;
use rama::http::Body;
use rama::http::HeaderValue;
use rama::http::Response;
use rama::http::StatusCode;
use serde::Serialize;
use tracing::error;

#[derive(Serialize)]
pub(super) struct BlockedResponse<'a> {
    pub(super) status: &'static str,
    pub(super) host: &'a str,
    pub(super) reason: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) decision: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) protocol: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) message: Option<String>,
}

pub(super) fn json_blocked(
    host: &str,
    reason: &str,
    details: Option<&PolicyDecisionDetails<'_>>,
) -> Response {
    let (message, decision, source, protocol, port) = details
        .map(|details| {
            (
                Some(blocked_message_with_policy(reason, details)),
                Some(details.decision.as_str()),
                Some(details.source.as_str()),
                Some(details.protocol.as_policy_protocol()),
                Some(details.port),
            )
        })
        .unwrap_or((None, None, None, None, None));
    let response = BlockedResponse {
        status: "blocked",
        host,
        reason,
        decision,
        source,
        protocol,
        port,
        message,
    };
    let mut resp = json_response(&response);
    *resp.status_mut() = StatusCode::FORBIDDEN;
    resp.headers_mut().insert(
        "x-proxy-error",
        HeaderValue::from_static(blocked_header_value(reason)),
    );
    resp
}

pub(super) fn blocked_text_with_details(
    reason: &str,
    details: &PolicyDecisionDetails<'_>,
) -> Response {
    blocked_text_response_with_policy(reason, details)
}

pub(super) async fn proxy_disabled_response(
    app_state: &NetworkProxyState,
    host: String,
    port: u16,
    client: Option<String>,
    method: Option<String>,
    protocol: NetworkProtocol,
    audit_endpoint_override: Option<(&str, u16)>,
) -> Response {
    let (audit_server_address, audit_server_port) =
        audit_endpoint_override.unwrap_or((host.as_str(), port));
    emit_http_block_decision_audit_event(
        app_state,
        BlockDecisionAuditEventArgs {
            source: crate::network_policy::NetworkDecisionSource::ProxyState,
            reason: REASON_PROXY_DISABLED,
            protocol,
            server_address: audit_server_address,
            server_port: audit_server_port,
            method: method.as_deref(),
            client_addr: client.as_deref(),
        },
    );

    let blocked_host = host.clone();
    let _ = app_state
        .record_blocked(BlockedRequest::new(BlockedRequestArgs {
            host: blocked_host,
            reason: REASON_PROXY_DISABLED.to_string(),
            client,
            method,
            mode: None,
            protocol: protocol.as_policy_protocol().to_string(),
            decision: Some("deny".to_string()),
            source: Some("proxy_state".to_string()),
            port: Some(port),
        }))
        .await;

    let details = PolicyDecisionDetails {
        decision: NetworkPolicyDecision::Deny,
        reason: REASON_PROXY_DISABLED,
        source: crate::network_policy::NetworkDecisionSource::ProxyState,
        protocol,
        host: &host,
        port,
    };
    text_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &blocked_message_with_policy(REASON_PROXY_DISABLED, &details),
    )
}

pub(super) fn internal_error(context: &str, err: impl std::fmt::Display) -> Response {
    error!("{context}: {err}");
    text_response(StatusCode::INTERNAL_SERVER_ERROR, "error")
}

pub(super) fn text_response(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(body.to_string())))
}

pub(super) fn emit_http_block_decision_audit_event(
    app_state: &NetworkProxyState,
    args: BlockDecisionAuditEventArgs<'_>,
) {
    emit_block_decision_audit_event(app_state, args);
}

pub(super) fn emit_http_allow_decision_audit_event(
    app_state: &NetworkProxyState,
    args: BlockDecisionAuditEventArgs<'_>,
) {
    emit_allow_decision_audit_event(app_state, args);
}
