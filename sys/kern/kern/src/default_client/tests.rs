use super::*;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;

#[test]
fn test_get_chaos_user_agent() {
    assert_eq!(DEFAULT_ORIGINATOR, "free_chaos");
    let user_agent = get_chaos_user_agent();
    let originator = originator().value.as_str();
    assert_eq!(user_agent, format!("{originator}/{CHAOS_VERSION}"));
}

#[tokio::test]
async fn test_create_client_sets_default_headers() {
    skip_if_no_network!();

    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let client = create_client();

    // Spin up a local mock server and capture a request.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let server_uri = server.uri();
    let resp = client
        .get(server_uri.as_str())
        .send()
        .await
        .expect("failed to send request");
    assert!(resp.status().is_success());

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    assert!(!requests.is_empty());
    let headers = &requests[0].headers;

    // originator header is set to the provided value
    let originator_header = headers
        .get("originator")
        .expect("originator header missing");
    assert_eq!(originator_header.to_str().unwrap(), originator().value);

    // User-Agent matches the computed Chaos UA for that originator
    let expected_ua = format!("{}/{CHAOS_VERSION}", originator().value);
    let ua_header = headers
        .get("user-agent")
        .expect("user-agent header missing");
    assert_eq!(ua_header.to_str().unwrap(), expected_ua);
}

#[test]
fn test_user_agent_sanitization() {
    let prefix = "free_chaos/0.0.0";
    for (suffix, expected) in [
        ("bad\rsuffix", "bad_suffix"),
        ("bad\0suffix", "bad_suffix"),
        ("café", "caf_"),
    ] {
        let (value, header) = sanitize_user_agent(format!("{prefix} ({suffix})"));
        assert_eq!(value, format!("{prefix} ({expected})"));
        assert_eq!(header.as_bytes(), value.as_bytes());
    }
    for candidate in ["", prefix, "free_chaos/0.0.0 (tab\tsuffix)"] {
        let (value, header) = sanitize_user_agent(candidate.to_string());
        assert_eq!(value, candidate);
        assert_eq!(header.as_bytes(), candidate.as_bytes());
    }
}
