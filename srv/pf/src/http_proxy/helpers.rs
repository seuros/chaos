use rama::extensions::ExtensionsRef;
use rama::http::HeaderMap;
use rama::http::HeaderName;
use rama::http::Request;
use rama::http::header;
use rama::http::headers::HeaderMapExt;
use rama::http::headers::Host;
use rama::net::http::RequestContext;
use rama::net::stream::SocketInfo;

pub(super) fn client_addr<T: ExtensionsRef>(input: &T) -> Option<String> {
    input
        .extensions()
        .get_ref::<SocketInfo>()
        .map(|info| info.peer_addr().to_string())
}

pub(super) fn validate_absolute_form_host_header(
    req: &Request,
    request_ctx: &RequestContext,
) -> Result<(), &'static str> {
    if req.uri().scheme_str().is_none() {
        return Ok(());
    }

    let Some(host_header) = req
        .headers()
        .typed_try_get::<Host>()
        .map_err(|_| "invalid Host header")?
    else {
        return Ok(());
    };

    if host_header.0.host != request_ctx.authority.host {
        return Err("Host header does not match request target");
    }

    if let Some(host_port) = host_header.0.port {
        if Some(host_port) != request_ctx.authority.port {
            return Err("Host header does not match request target");
        }
        return Ok(());
    }

    if !request_ctx.authority_has_default_port() {
        return Err("Host header does not match request target");
    }

    Ok(())
}

pub(super) fn remove_hop_by_hop_request_headers(headers: &mut HeaderMap) {
    while let Some(raw_connection) = headers.get(header::CONNECTION).cloned() {
        headers.remove(header::CONNECTION);
        if let Ok(raw_connection) = raw_connection.to_str() {
            let connection_headers: Vec<String> = raw_connection
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            for token in connection_headers {
                if let Ok(name) = HeaderName::from_bytes(token.as_bytes()) {
                    headers.remove(name);
                }
            }
        }
    }
    for name in [
        &header::KEEP_ALIVE,
        &header::PROXY_CONNECTION,
        &header::PROXY_AUTHORIZATION,
        &header::TRAILER,
        &header::TRANSFER_ENCODING,
        &header::UPGRADE,
    ] {
        headers.remove(name);
    }

    // codespell:ignore te,TE
    // 0x74,0x65 is ASCII "te" (the HTTP TE hop-by-hop header).
    if let Ok(short_hop_header_name) = HeaderName::from_bytes(&[0x74, 0x65]) {
        headers.remove(short_hop_header_name);
    }
}
