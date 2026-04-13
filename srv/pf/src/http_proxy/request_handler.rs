use super::helpers::client_addr;
use super::helpers::remove_hop_by_hop_request_headers;
use super::helpers::validate_absolute_form_host_header;
use super::responses::blocked_text_with_details;
use super::responses::emit_http_allow_decision_audit_event;
use super::responses::emit_http_block_decision_audit_event;
use super::responses::internal_error;
use super::responses::json_blocked;
use super::responses::proxy_disabled_response;
use super::responses::text_response;
use super::tunnel::proxy_via_unix_socket;
use crate::config::NetworkMode;
use crate::network_policy::BlockDecisionAuditEventArgs;
use crate::network_policy::NetworkDecision;
use crate::network_policy::NetworkDecisionSource;
use crate::network_policy::NetworkPolicyDecider;
use crate::network_policy::NetworkPolicyDecision;
use crate::network_policy::NetworkPolicyRequest;
use crate::network_policy::NetworkPolicyRequestArgs;
use crate::network_policy::NetworkProtocol;
use crate::network_policy::evaluate_host_policy;
use crate::policy::normalize_host;
use crate::reasons::REASON_METHOD_NOT_ALLOWED;
use crate::reasons::REASON_MITM_REQUIRED;
use crate::reasons::REASON_NOT_ALLOWED;
use crate::reasons::REASON_UNIX_SOCKET_UNSUPPORTED;
use crate::responses::PolicyDecisionDetails;
use crate::runtime::unix_socket_permissions_supported;
use crate::state::BlockedRequest;
use crate::state::BlockedRequestArgs;
use crate::state::NetworkProxyState;
use crate::upstream::UpstreamClient;
use rama::Service;
use rama::extensions::ExtensionsRef;
use rama::http::Request;
use rama::http::Response;
use rama::http::StatusCode;
use rama::http::header;
use rama::http::layer::upgrade::DefaultHttpProxyConnectReplyService;
use rama::net::http::RequestContext;
use rama::net::proxy::ProxyTarget;
use std::convert::Infallible;
use std::sync::Arc;
use tracing::error;
use tracing::info;
use tracing::warn;

pub(super) async fn http_connect_accept(
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    req: Request,
) -> Result<(Response, Request), Response> {
    let app_state = req
        .extensions()
        .get_arc::<NetworkProxyState>()
        .ok_or_else(|| text_response(StatusCode::INTERNAL_SERVER_ERROR, "missing state"))?;

    let authority = match RequestContext::try_from(&req).map(|ctx| ctx.host_with_port()) {
        Ok(authority) => authority,
        Err(err) => {
            warn!("CONNECT missing authority: {err}");
            return Err(text_response(StatusCode::BAD_REQUEST, "missing authority"));
        }
    };

    let host = normalize_host(&authority.host.to_string());
    if host.is_empty() {
        return Err(text_response(StatusCode::BAD_REQUEST, "invalid host"));
    }

    let client = client_addr(&req);
    let enabled = app_state
        .enabled()
        .await
        .map_err(|err| internal_error("failed to read enabled state", err))?;
    if !enabled {
        let client = client.as_deref().unwrap_or_default();
        warn!("CONNECT blocked; proxy disabled (client={client}, host={host})");
        return Err(proxy_disabled_response(
            &app_state,
            host,
            authority.port,
            client_addr(&req),
            Some("CONNECT".to_string()),
            NetworkProtocol::HttpsConnect,
            /*audit_endpoint_override*/ None,
        )
        .await);
    }

    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::HttpsConnect,
        host: host.clone(),
        port: authority.port,
        client_addr: client.clone(),
        method: Some("CONNECT".to_string()),
        command: None,
        exec_policy_hint: None,
    });

    match evaluate_host_policy(&app_state, policy_decider.as_ref(), &request).await {
        Ok(NetworkDecision::Deny {
            reason,
            source,
            decision,
        }) => {
            let details = PolicyDecisionDetails {
                decision,
                reason: &reason,
                source,
                protocol: NetworkProtocol::HttpsConnect,
                host: &host,
                port: authority.port,
            };
            let _ = app_state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: reason.clone(),
                    client: client.clone(),
                    method: Some("CONNECT".to_string()),
                    mode: None,
                    protocol: "http-connect".to_string(),
                    decision: Some(details.decision.as_str().to_string()),
                    source: Some(details.source.as_str().to_string()),
                    port: Some(authority.port),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("CONNECT blocked (client={client}, host={host}, reason={reason})");
            return Err(blocked_text_with_details(&reason, &details));
        }
        Ok(NetworkDecision::Allow) => {
            let client = client.as_deref().unwrap_or_default();
            info!("CONNECT allowed (client={client}, host={host})");
        }
        Err(err) => {
            error!("failed to evaluate host for CONNECT {host}: {err}");
            return Err(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"));
        }
    }

    let mode = app_state
        .network_mode()
        .await
        .map_err(|err| internal_error("failed to read network mode", err))?;

    let mitm_state = match app_state.mitm_state().await {
        Ok(state) => state,
        Err(err) => {
            error!("failed to load MITM state: {err}");
            return Err(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"));
        }
    };

    if mode == NetworkMode::Limited && mitm_state.is_none() {
        // Limited mode is designed to be read-only. Without MITM, a CONNECT tunnel would hide the
        // inner HTTP method/headers from the proxy, effectively bypassing method policy.
        emit_http_block_decision_audit_event(
            &app_state,
            BlockDecisionAuditEventArgs {
                source: NetworkDecisionSource::ModeGuard,
                reason: REASON_MITM_REQUIRED,
                protocol: NetworkProtocol::HttpsConnect,
                server_address: host.as_str(),
                server_port: authority.port,
                method: Some("CONNECT"),
                client_addr: client.as_deref(),
            },
        );
        let details = PolicyDecisionDetails {
            decision: NetworkPolicyDecision::Deny,
            reason: REASON_MITM_REQUIRED,
            source: NetworkDecisionSource::ModeGuard,
            protocol: NetworkProtocol::HttpsConnect,
            host: &host,
            port: authority.port,
        };
        let _ = app_state
            .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                host: host.clone(),
                reason: REASON_MITM_REQUIRED.to_string(),
                client: client.clone(),
                method: Some("CONNECT".to_string()),
                mode: Some(NetworkMode::Limited),
                protocol: "http-connect".to_string(),
                decision: Some(details.decision.as_str().to_string()),
                source: Some(details.source.as_str().to_string()),
                port: Some(authority.port),
            }))
            .await;
        let client = client.as_deref().unwrap_or_default();
        warn!(
            "CONNECT blocked; MITM required for read-only HTTPS in limited mode (client={client}, host={host}, mode=limited, allowed_methods=GET, HEAD, OPTIONS)"
        );
        return Err(blocked_text_with_details(REASON_MITM_REQUIRED, &details));
    }

    req.extensions().insert(ProxyTarget(authority));
    req.extensions().insert(mode);
    if let Some(mitm_state) = mitm_state {
        req.extensions().insert_arc(mitm_state);
    }

    DefaultHttpProxyConnectReplyService::new().serve(req).await
}

pub(super) async fn http_plain_proxy(
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    mut req: Request,
) -> Result<Response, Infallible> {
    let app_state = match req.extensions().get_arc::<NetworkProxyState>() {
        Some(state) => state,
        None => {
            error!("missing app state");
            return Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"));
        }
    };
    let client = client_addr(&req);
    let method_allowed = match app_state
        .method_allowed(req.method().as_str())
        .await
        .map_err(|err| internal_error("failed to evaluate method policy", err))
    {
        Ok(allowed) => allowed,
        Err(resp) => return Ok(resp),
    };

    // `x-unix-socket` is an escape hatch for talking to local daemons. We keep it tightly scoped:
    // macOS-only + explicit allowlist by default, to avoid turning the proxy into a general local
    // capability escalation mechanism.
    if let Some(unix_socket_header) = req.headers().get("x-unix-socket") {
        let socket_path = match unix_socket_header.to_str() {
            Ok(value) => value.to_string(),
            Err(_) => {
                warn!("invalid x-unix-socket header value (non-UTF8)");
                return Ok(text_response(
                    StatusCode::BAD_REQUEST,
                    "invalid x-unix-socket header",
                ));
            }
        };
        let enabled = match app_state
            .enabled()
            .await
            .map_err(|err| internal_error("failed to read enabled state", err))
        {
            Ok(enabled) => enabled,
            Err(resp) => return Ok(resp),
        };
        if !enabled {
            let client = client.as_deref().unwrap_or_default();
            warn!("unix socket blocked; proxy disabled (client={client}, path={socket_path})");
            return Ok(proxy_disabled_response(
                &app_state,
                socket_path,
                /*port*/ 0,
                client_addr(&req),
                Some(req.method().as_str().to_string()),
                NetworkProtocol::Http,
                Some(("unix-socket", 0)),
            )
            .await);
        }
        if !method_allowed {
            emit_http_block_decision_audit_event(
                &app_state,
                BlockDecisionAuditEventArgs {
                    source: NetworkDecisionSource::ModeGuard,
                    reason: REASON_METHOD_NOT_ALLOWED,
                    protocol: NetworkProtocol::Http,
                    server_address: "unix-socket",
                    server_port: 0,
                    method: Some(req.method().as_str()),
                    client_addr: client.as_deref(),
                },
            );
            let client = client.as_deref().unwrap_or_default();
            let method = req.method();
            warn!(
                "unix socket blocked by method policy (client={client}, method={method}, mode=limited, allowed_methods=GET, HEAD, OPTIONS)"
            );
            return Ok(json_blocked(
                "unix-socket",
                REASON_METHOD_NOT_ALLOWED,
                /*details*/ None,
            ));
        }

        if !unix_socket_permissions_supported() {
            emit_http_block_decision_audit_event(
                &app_state,
                BlockDecisionAuditEventArgs {
                    source: NetworkDecisionSource::ProxyState,
                    reason: REASON_UNIX_SOCKET_UNSUPPORTED,
                    protocol: NetworkProtocol::Http,
                    server_address: "unix-socket",
                    server_port: 0,
                    method: Some(req.method().as_str()),
                    client_addr: client.as_deref(),
                },
            );
            warn!("unix socket proxy unsupported on this platform (path={socket_path})");
            return Ok(text_response(
                StatusCode::NOT_IMPLEMENTED,
                "unix sockets unsupported",
            ));
        }

        return match app_state.is_unix_socket_allowed(&socket_path).await {
            Ok(true) => {
                emit_http_allow_decision_audit_event(
                    &app_state,
                    BlockDecisionAuditEventArgs {
                        source: NetworkDecisionSource::ProxyState,
                        reason: "allow",
                        protocol: NetworkProtocol::Http,
                        server_address: "unix-socket",
                        server_port: 0,
                        method: Some(req.method().as_str()),
                        client_addr: client.as_deref(),
                    },
                );
                let client = client.as_deref().unwrap_or_default();
                info!("unix socket allowed (client={client}, path={socket_path})");
                match proxy_via_unix_socket(req, &socket_path).await {
                    Ok(resp) => Ok(resp),
                    Err(err) => {
                        warn!("unix socket proxy failed: {err}");
                        Ok(text_response(
                            StatusCode::BAD_GATEWAY,
                            "unix socket proxy failed",
                        ))
                    }
                }
            }
            Ok(false) => {
                emit_http_block_decision_audit_event(
                    &app_state,
                    BlockDecisionAuditEventArgs {
                        source: NetworkDecisionSource::ProxyState,
                        reason: REASON_NOT_ALLOWED,
                        protocol: NetworkProtocol::Http,
                        server_address: "unix-socket",
                        server_port: 0,
                        method: Some(req.method().as_str()),
                        client_addr: client.as_deref(),
                    },
                );
                let client = client.as_deref().unwrap_or_default();
                warn!("unix socket blocked (client={client}, path={socket_path})");
                Ok(json_blocked(
                    "unix-socket",
                    REASON_NOT_ALLOWED,
                    /*details*/ None,
                ))
            }
            Err(err) => {
                warn!("unix socket check failed: {err}");
                Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"))
            }
        };
    }

    let request_ctx = match RequestContext::try_from(&req) {
        Ok(request_ctx) => request_ctx,
        Err(err) => {
            warn!("missing host: {err}");
            return Ok(text_response(StatusCode::BAD_REQUEST, "missing host"));
        }
    };
    let authority = request_ctx.host_with_port();
    let host = normalize_host(&authority.host.to_string());
    let port = authority.port;
    if let Err(reason) = validate_absolute_form_host_header(&req, &request_ctx) {
        let client = client.as_deref().unwrap_or_default();
        let host_header = req
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<missing>");
        warn!(
            "request rejected due to mismatched Host header (client={client}, target={host}:{port}, host_header={host_header}, reason={reason})"
        );
        return Ok(text_response(StatusCode::BAD_REQUEST, reason));
    }
    let enabled = match app_state
        .enabled()
        .await
        .map_err(|err| internal_error("failed to read enabled state", err))
    {
        Ok(enabled) => enabled,
        Err(resp) => return Ok(resp),
    };
    if !enabled {
        let client = client.as_deref().unwrap_or_default();
        let method = req.method();
        warn!("request blocked; proxy disabled (client={client}, host={host}, method={method})");
        return Ok(proxy_disabled_response(
            &app_state,
            host,
            port,
            client_addr(&req),
            Some(req.method().as_str().to_string()),
            NetworkProtocol::Http,
            /*audit_endpoint_override*/ None,
        )
        .await);
    }

    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Http,
        host: host.clone(),
        port,
        client_addr: client.clone(),
        method: Some(req.method().as_str().to_string()),
        command: None,
        exec_policy_hint: None,
    });

    match evaluate_host_policy(&app_state, policy_decider.as_ref(), &request).await {
        Ok(NetworkDecision::Deny {
            reason,
            source,
            decision,
        }) => {
            let details = PolicyDecisionDetails {
                decision,
                reason: &reason,
                source,
                protocol: NetworkProtocol::Http,
                host: &host,
                port,
            };
            let _ = app_state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: reason.clone(),
                    client: client.clone(),
                    method: Some(req.method().as_str().to_string()),
                    mode: None,
                    protocol: "http".to_string(),
                    decision: Some(details.decision.as_str().to_string()),
                    source: Some(details.source.as_str().to_string()),
                    port: Some(port),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("request blocked (client={client}, host={host}, reason={reason})");
            return Ok(json_blocked(&host, &reason, Some(&details)));
        }
        Ok(NetworkDecision::Allow) => {}
        Err(err) => {
            error!("failed to evaluate host for {host}: {err}");
            return Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"));
        }
    }

    if !method_allowed {
        emit_http_block_decision_audit_event(
            &app_state,
            BlockDecisionAuditEventArgs {
                source: NetworkDecisionSource::ModeGuard,
                reason: REASON_METHOD_NOT_ALLOWED,
                protocol: NetworkProtocol::Http,
                server_address: host.as_str(),
                server_port: port,
                method: Some(req.method().as_str()),
                client_addr: client.as_deref(),
            },
        );
        let details = PolicyDecisionDetails {
            decision: NetworkPolicyDecision::Deny,
            reason: REASON_METHOD_NOT_ALLOWED,
            source: NetworkDecisionSource::ModeGuard,
            protocol: NetworkProtocol::Http,
            host: &host,
            port,
        };
        let _ = app_state
            .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                host: host.clone(),
                reason: REASON_METHOD_NOT_ALLOWED.to_string(),
                client: client.clone(),
                method: Some(req.method().as_str().to_string()),
                mode: Some(NetworkMode::Limited),
                protocol: "http".to_string(),
                decision: Some(details.decision.as_str().to_string()),
                source: Some(details.source.as_str().to_string()),
                port: Some(port),
            }))
            .await;
        let client = client.as_deref().unwrap_or_default();
        let method = req.method();
        warn!(
            "request blocked by method policy (client={client}, host={host}, method={method}, mode=limited, allowed_methods=GET, HEAD, OPTIONS)"
        );
        return Ok(json_blocked(
            &host,
            REASON_METHOD_NOT_ALLOWED,
            Some(&details),
        ));
    }

    let client = client.as_deref().unwrap_or_default();
    let method = req.method();
    info!("request allowed (client={client}, host={host}, method={method})");

    let allow_upstream_proxy = match app_state
        .allow_upstream_proxy()
        .await
        .map_err(|err| internal_error("failed to read upstream proxy config", err))
    {
        Ok(allow) => allow,
        Err(resp) => return Ok(resp),
    };
    let client = if allow_upstream_proxy {
        UpstreamClient::from_env_proxy()
    } else {
        UpstreamClient::direct()
    };

    // Strip hop-by-hop headers only after extracting metadata used for policy correlation.
    remove_hop_by_hop_request_headers(req.headers_mut());
    match client.serve(req).await {
        Ok(resp) => Ok(resp),
        Err(err) => {
            warn!("upstream request failed: {err}");
            Ok(text_response(StatusCode::BAD_GATEWAY, "upstream failure"))
        }
    }
}
