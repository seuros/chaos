use super::*;
use base64::Engine;
use pretty_assertions::assert_eq;

#[test]
fn map_api_error_maps_server_overloaded() {
    let err = map_api_error(ApiError::ServerOverloaded);
    assert!(matches!(err, ChaosErr::ServerOverloaded));
}

#[test]
fn map_api_error_maps_server_overloaded_from_503_body() {
    let body = serde_json::json!({
        "error": {
            "code": "server_is_overloaded"
        }
    })
    .to_string();
    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::SERVICE_UNAVAILABLE,
        url: Some("http://example.com/v1/responses".to_string()),
        headers: None,
        body: Some(body),
    }));

    assert!(matches!(err, ChaosErr::ServerOverloaded));
}

#[test]
fn map_api_error_maps_usage_limit_limit_name_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACTIVE_LIMIT_HEADER,
        http::HeaderValue::from_static("chaos_other"),
    );
    headers.insert(
        "x-chaos-other-limit-name",
        http::HeaderValue::from_static("chaos_other"),
    );
    let body = serde_json::json!({
        "error": {
            "type": "usage_limit_reached",
            "plan_type": "pro",
        }
    })
    .to_string();
    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::TOO_MANY_REQUESTS,
        url: Some("http://example.com/v1/responses".to_string()),
        headers: Some(headers),
        body: Some(body),
    }));

    let ChaosErr::UsageLimitReached(usage_limit) = err else {
        panic!("expected ChaosErr::UsageLimitReached, got {err:?}");
    };
    assert_eq!(
        usage_limit
            .rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.limit_name.as_deref()),
        Some("chaos_other")
    );
}

#[test]
fn map_api_error_does_not_fallback_limit_name_to_limit_id() {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACTIVE_LIMIT_HEADER,
        http::HeaderValue::from_static("chaos_other"),
    );
    let body = serde_json::json!({
        "error": {
            "type": "usage_limit_reached",
            "plan_type": "pro",
        }
    })
    .to_string();
    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::TOO_MANY_REQUESTS,
        url: Some("http://example.com/v1/responses".to_string()),
        headers: Some(headers),
        body: Some(body),
    }));

    let ChaosErr::UsageLimitReached(usage_limit) = err else {
        panic!("expected ChaosErr::UsageLimitReached, got {err:?}");
    };
    assert_eq!(
        usage_limit
            .rate_limits
            .as_ref()
            .and_then(|snapshot| snapshot.limit_name.as_deref()),
        None
    );
}

#[test]
fn map_api_error_extracts_identity_auth_details_from_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(REQUEST_ID_HEADER, http::HeaderValue::from_static("req-401"));
    headers.insert(CF_RAY_HEADER, http::HeaderValue::from_static("ray-401"));
    headers.insert(
        X_OPENAI_AUTHORIZATION_ERROR_HEADER,
        http::HeaderValue::from_static("missing_authorization_header"),
    );
    let x_error_json =
        base64::engine::general_purpose::STANDARD.encode(r#"{"error":{"code":"token_expired"}}"#);
    headers.insert(
        X_ERROR_JSON_HEADER,
        http::HeaderValue::from_str(&x_error_json).expect("valid x-error-json header"),
    );

    let err = map_api_error(ApiError::Transport(TransportError::Http {
        status: http::StatusCode::UNAUTHORIZED,
        url: Some(chaos_services::openai::CHATGPT_MODELS_URL.to_string()),
        headers: Some(headers),
        body: Some(r#"{"detail":"Unauthorized"}"#.to_string()),
    }));

    let ChaosErr::UnexpectedStatus(err) = err else {
        panic!("expected ChaosErr::UnexpectedStatus, got {err:?}");
    };
    assert_eq!(err.request_id.as_deref(), Some("req-401"));
    assert_eq!(err.cf_ray.as_deref(), Some("ray-401"));
    assert_eq!(
        err.identity_authorization_error.as_deref(),
        Some("missing_authorization_header")
    );
    assert_eq!(err.identity_error_code.as_deref(), Some("token_expired"));
}

#[test]
fn map_api_error_network_error_becomes_connection_failed() {
    let err = map_api_error(ApiError::Transport(TransportError::Network(
        "failed to (tcp) connect to any resolved IP address".to_string(),
    )));
    assert!(matches!(err, ChaosErr::ConnectionFailed(_)));
    assert!(err.is_retryable());
    assert!(err.to_string().contains("Connection failed"));
}

#[test]
fn abi_transport_status_zero_becomes_network_error() {
    use chaos_abi::AbiError;
    let api_err = abi_error_to_api_error(AbiError::Transport {
        status: 0,
        message: "tcp connect refused".to_string(),
    });
    assert!(matches!(
        api_err,
        ApiError::Transport(TransportError::Network(_))
    ));
    let chaos = map_api_error(api_err);
    assert!(matches!(chaos, ChaosErr::ConnectionFailed(_)));
}

#[test]
fn core_auth_provider_reports_when_auth_header_will_attach() {
    let auth = CoreAuthProvider {
        token: Some("access-token".to_string()),
        account_id: None,
    };

    assert!(auth.auth_header_attached());
    assert_eq!(auth.auth_header_name(), Some("authorization"));
}

// Single-input preflight matrix: the same provider table covers OpenAI (OAuth
// fallback), an env-key provider with unset env, an env-key provider with set
// env, a baked bearer token, and a self-hosted (no-auth) provider.
#[test]
fn auth_provider_from_auth_preflight_matrix() {
    use crate::model_provider_info::OPENAI_PROVIDER_ID;

    let openai_missing = ModelProviderInfo::create_openai_provider(None);
    let Err(err) = auth_provider_from_auth(None, &openai_missing) else {
        panic!("OpenAI without cached login must fail preflight");
    };
    let ChaosErr::ProviderAuthMissing(info) = err else {
        panic!("expected ProviderAuthMissing, got {err:?}");
    };
    assert_eq!(info.provider_id, OPENAI_PROVIDER_ID);
    assert_eq!(info.env_key, None);
    assert!(info.supports_oauth, "OpenAI should offer OAuth fallback");

    let xai_var = "CHAOS_TEST_XAI_KEY_UNSET";
    // SAFETY: test-only; ensures a clean env slot for the env-key case.
    unsafe {
        std::env::remove_var(xai_var);
    }
    let xai_missing = ModelProviderInfo {
        name: "xAI".into(),
        base_url: Some("https://api.x.ai/v1".into()),
        env_key: Some(xai_var.into()),
        env_key_instructions: Some("Create a key at https://x.ai/api.".into()),
        experimental_bearer_token: None,
        wire_api: crate::model_provider_info::WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    };
    let Err(err) = auth_provider_from_auth(None, &xai_missing) else {
        panic!("missing env key must surface preflight error");
    };
    let ChaosErr::ProviderAuthMissing(info) = err else {
        panic!("expected ProviderAuthMissing, got {err:?}");
    };
    assert_eq!(info.provider_id, "xAI");
    assert_eq!(info.env_key.as_deref(), Some(xai_var));
    assert!(!info.supports_oauth);
    assert!(info.to_string().contains(xai_var));

    unsafe {
        std::env::set_var(xai_var, "live-key");
    }
    let xai_set = xai_missing.clone();
    let auth = auth_provider_from_auth(None, &xai_set).expect("env key set must succeed");
    assert_eq!(auth.token.as_deref(), Some("live-key"));
    unsafe {
        std::env::remove_var(xai_var);
    }

    let bearer = ModelProviderInfo {
        experimental_bearer_token: Some("baked-token".into()),
        env_key: None,
        ..xai_missing
    };
    let auth = auth_provider_from_auth(None, &bearer).expect("bearer must satisfy preflight");
    assert_eq!(auth.token.as_deref(), Some("baked-token"));

    let minimax_var = "CHAOS_TEST_MINIMAX_KEY_UNSET";
    unsafe {
        std::env::remove_var(minimax_var);
    }
    let minimax = ModelProviderInfo {
        name: "MiniMax".into(),
        base_url: Some("https://api.minimax.io/anthropic".into()),
        env_key: Some(minimax_var.into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: crate::model_provider_info::WireApi::Auto,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    };
    let Err(ChaosErr::ProviderAuthMissing(info)) = auth_provider_from_auth(None, &minimax) else {
        panic!("anthropic-wire provider without env must preflight-fail");
    };
    assert_eq!(
        info.provider_id,
        crate::model_provider_info::ANTHROPIC_PROVIDER_ID
    );

    let ollama = ModelProviderInfo {
        name: "Ollama".into(),
        base_url: Some("http://localhost:11434/v1".into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: crate::model_provider_info::WireApi::ChatCompletions,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    };
    let auth =
        auth_provider_from_auth(None, &ollama).expect("self-hosted provider needs no credentials");
    assert!(auth.token.is_none());
}
