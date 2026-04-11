use super::*;
use crate::exec::StreamOutput;
use chaos_ipc::protocol::RateLimitWindow;
use http::StatusCode;
use jiff::Timestamp;
use jiff::ToSpan;
use pretty_assertions::assert_eq;

fn rate_limit_snapshot() -> RateLimitSnapshot {
    let primary_reset_at = "2024-01-01T01:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_second();
    let secondary_reset_at = "2024-01-01T02:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_second();
    RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 50.0,
            window_minutes: Some(60),
            resets_at: Some(primary_reset_at),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 30.0,
            window_minutes: Some(120),
            resets_at: Some(secondary_reset_at),
        }),
        credits: None,
        plan_type: None,
    }
}

fn with_now_override<T>(now: Timestamp, f: impl FnOnce() -> T) -> T {
    NOW_OVERRIDE.with(|cell| {
        *cell.borrow_mut() = Some(now);
        let result = f();
        *cell.borrow_mut() = None;
        result
    })
}

#[test]
fn usage_limit_reached_without_reset_reports_unknown_refill() {
    let err = UsageLimitReachedError {
        resets_at: None,
        rate_limits: Some(Box::new(rate_limit_snapshot())),
    };
    assert_eq!(
        err.to_string(),
        "Hallucination overdose. Refill ETA unknown."
    );
}

#[test]
fn usage_limit_reached_with_reset_announces_next_refill() {
    let base: Timestamp = "2024-01-01T00:00:00Z".parse().unwrap();
    let resets_at = base.checked_add(1.hours()).unwrap();
    with_now_override(base, move || {
        let expected_time = format_retry_timestamp(&resets_at);
        let err = UsageLimitReachedError {
            resets_at: Some(resets_at),
            rate_limits: Some(Box::new(rate_limit_snapshot())),
        };
        let expected = format!("Hallucination overdose. Next refill: {expected_time}.");
        assert_eq!(err.to_string(), expected);
    });
}

#[test]
fn usage_limit_reached_reports_named_limit_when_present() {
    let base: Timestamp = "2024-01-01T00:00:00Z".parse().unwrap();
    let resets_at = base.checked_add(3.hours()).unwrap();
    with_now_override(base, move || {
        let expected_time = format_retry_timestamp(&resets_at);
        let err = UsageLimitReachedError {
            resets_at: Some(resets_at),
            rate_limits: Some(Box::new(RateLimitSnapshot {
                limit_id: Some("gpt-5-weekly".to_string()),
                limit_name: Some("gpt-5-weekly".to_string()),
                ..rate_limit_snapshot()
            })),
        };
        let expected =
            format!("Hallucination overdose on gpt-5-weekly. Next refill: {expected_time}.");
        assert_eq!(err.to_string(), expected);
    });
}

#[test]
fn usage_limit_reached_suppresses_chaos_branded_limit_name() {
    let err = UsageLimitReachedError {
        resets_at: None,
        rate_limits: Some(Box::new(RateLimitSnapshot {
            limit_id: Some("chaos".to_string()),
            limit_name: Some("chaos".to_string()),
            ..rate_limit_snapshot()
        })),
    };
    assert_eq!(
        err.to_string(),
        "Hallucination overdose. Refill ETA unknown."
    );
}

#[test]
fn server_overloaded_maps_to_protocol() {
    let err = ChaosErr::ServerOverloaded;
    assert_eq!(err.to_chaos_ipc_error(), ChaosErrorInfo::ServerOverloaded);
}

#[test]
fn sandbox_denied_uses_aggregated_output_when_stderr_empty() {
    let output = ExecToolCallOutput {
        exit_code: 77,
        stdout: StreamOutput::new(String::new()),
        stderr: StreamOutput::new(String::new()),
        aggregated_output: StreamOutput::new("aggregate detail".to_string()),
        duration: Duration::from_millis(10),
        timed_out: false,
    };
    let err = ChaosErr::Sandbox(SandboxErr::Denied {
        output: Box::new(output),
        network_policy_decision: None,
    });
    assert_eq!(get_error_message_ui(&err), "aggregate detail");
}

#[test]
fn sandbox_denied_reports_both_streams_when_available() {
    let output = ExecToolCallOutput {
        exit_code: 9,
        stdout: StreamOutput::new("stdout detail".to_string()),
        stderr: StreamOutput::new("stderr detail".to_string()),
        aggregated_output: StreamOutput::new(String::new()),
        duration: Duration::from_millis(10),
        timed_out: false,
    };
    let err = ChaosErr::Sandbox(SandboxErr::Denied {
        output: Box::new(output),
        network_policy_decision: None,
    });
    assert_eq!(get_error_message_ui(&err), "stderr detail\nstdout detail");
}

#[test]
fn sandbox_denied_reports_stdout_when_no_stderr() {
    let output = ExecToolCallOutput {
        exit_code: 11,
        stdout: StreamOutput::new("stdout only".to_string()),
        stderr: StreamOutput::new(String::new()),
        aggregated_output: StreamOutput::new(String::new()),
        duration: Duration::from_millis(8),
        timed_out: false,
    };
    let err = ChaosErr::Sandbox(SandboxErr::Denied {
        output: Box::new(output),
        network_policy_decision: None,
    });
    assert_eq!(get_error_message_ui(&err), "stdout only");
}

#[test]
fn to_error_event_handles_response_stream_failed() {
    let source: Box<dyn std::error::Error + Send + Sync> =
        Box::new(std::io::Error::other("stream connection lost"));
    let err = ChaosErr::ResponseStreamFailed(ResponseStreamFailed {
        source,
        request_id: Some("req-123".to_string()),
    });

    let event = err.to_error_event(Some("prefix".to_string()));

    assert!(event.message.contains("prefix:"));
    assert!(event.message.contains("req-123"));
    assert_eq!(
        event.chaos_error_info,
        Some(ChaosErrorInfo::ResponseStreamConnectionFailed {
            http_status_code: None
        })
    );
}

#[test]
fn sandbox_denied_reports_exit_code_when_no_output_available() {
    let output = ExecToolCallOutput {
        exit_code: 13,
        stdout: StreamOutput::new(String::new()),
        stderr: StreamOutput::new(String::new()),
        aggregated_output: StreamOutput::new(String::new()),
        duration: Duration::from_millis(5),
        timed_out: false,
    };
    let err = ChaosErr::Sandbox(SandboxErr::Denied {
        output: Box::new(output),
        network_policy_decision: None,
    });
    assert_eq!(
        get_error_message_ui(&err),
        "command failed inside sandbox with exit code 13"
    );
}

#[test]
fn unexpected_status_cloudflare_html_is_simplified() {
    let err = UnexpectedResponseError {
        status: StatusCode::FORBIDDEN,
        body: "<html><body>Cloudflare error: Sorry, you have been blocked</body></html>"
            .to_string(),
        url: Some("http://example.com/blocked".to_string()),
        cf_ray: Some("ray-id".to_string()),
        request_id: None,
        identity_authorization_error: None,
        identity_error_code: None,
    };
    let status = StatusCode::FORBIDDEN.to_string();
    let url = "http://example.com/blocked";
    assert_eq!(
        err.to_string(),
        format!("{CLOUDFLARE_BLOCKED_MESSAGE} (status {status}), url: {url}, cf-ray: ray-id")
    );
}

#[test]
fn unexpected_status_non_html_is_unchanged() {
    let err = UnexpectedResponseError {
        status: StatusCode::FORBIDDEN,
        body: "plain text error".to_string(),
        url: Some("http://example.com/plain".to_string()),
        cf_ray: None,
        request_id: None,
        identity_authorization_error: None,
        identity_error_code: None,
    };
    let status = StatusCode::FORBIDDEN.to_string();
    let url = "http://example.com/plain";
    assert_eq!(
        err.to_string(),
        format!("unexpected status {status}: plain text error, url: {url}")
    );
}

#[test]
fn unexpected_status_prefers_error_message_when_present() {
    let err = UnexpectedResponseError {
        status: StatusCode::UNAUTHORIZED,
        body: r#"{"error":{"message":"Workspace is not authorized in this region."},"status":401}"#
            .to_string(),
        url: Some(chaos_services::openai::CHATGPT_RESPONSES_URL.to_string()),
        cf_ray: None,
        request_id: Some("req-123".to_string()),
        identity_authorization_error: None,
        identity_error_code: None,
    };
    let status = StatusCode::UNAUTHORIZED.to_string();
    let responses_url = chaos_services::openai::CHATGPT_RESPONSES_URL;
    assert_eq!(
        err.to_string(),
        format!(
            "unexpected status {status}: Workspace is not authorized in this region., url: {responses_url}, request id: req-123"
        )
    );
}

#[test]
fn unexpected_status_truncates_long_body_with_ellipsis() {
    let long_body = "x".repeat(UNEXPECTED_RESPONSE_BODY_MAX_BYTES + 10);
    let err = UnexpectedResponseError {
        status: StatusCode::BAD_GATEWAY,
        body: long_body,
        url: Some("http://example.com/long".to_string()),
        cf_ray: None,
        request_id: Some("req-long".to_string()),
        identity_authorization_error: None,
        identity_error_code: None,
    };
    let status = StatusCode::BAD_GATEWAY.to_string();
    let expected_body = format!("{}...", "x".repeat(UNEXPECTED_RESPONSE_BODY_MAX_BYTES));
    assert_eq!(
        err.to_string(),
        format!(
            "unexpected status {status}: {expected_body}, url: http://example.com/long, request id: req-long"
        )
    );
}

#[test]
fn unexpected_status_includes_cf_ray_and_request_id() {
    let err = UnexpectedResponseError {
        status: StatusCode::UNAUTHORIZED,
        body: "plain text error".to_string(),
        url: Some(chaos_services::openai::CHATGPT_RESPONSES_URL.to_string()),
        cf_ray: Some("9c81f9f18f2fa49d-LHR".to_string()),
        request_id: Some("req-xyz".to_string()),
        identity_authorization_error: None,
        identity_error_code: None,
    };
    let status = StatusCode::UNAUTHORIZED.to_string();
    let responses_url = chaos_services::openai::CHATGPT_RESPONSES_URL;
    assert_eq!(
        err.to_string(),
        format!(
            "unexpected status {status}: plain text error, url: {responses_url}, cf-ray: 9c81f9f18f2fa49d-LHR, request id: req-xyz"
        )
    );
}

#[test]
fn unexpected_status_includes_identity_auth_details() {
    let err = UnexpectedResponseError {
        status: StatusCode::UNAUTHORIZED,
        body: "plain text error".to_string(),
        url: Some(chaos_services::openai::CHATGPT_MODELS_URL.to_string()),
        cf_ray: Some("cf-ray-auth-401-test".to_string()),
        request_id: Some("req-auth".to_string()),
        identity_authorization_error: Some("missing_authorization_header".to_string()),
        identity_error_code: Some("token_expired".to_string()),
    };
    let status = StatusCode::UNAUTHORIZED.to_string();
    let models_url = chaos_services::openai::CHATGPT_MODELS_URL;
    assert_eq!(
        err.to_string(),
        format!(
            "unexpected status {status}: plain text error, url: {models_url}, cf-ray: cf-ray-auth-401-test, request id: req-auth, auth error: missing_authorization_header, auth error code: token_expired"
        )
    );
}
