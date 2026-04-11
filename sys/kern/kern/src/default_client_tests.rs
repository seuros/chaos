use super::*;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;

#[test]
fn test_get_chaos_user_agent() {
    let user_agent = get_chaos_user_agent();
    let originator = originator().value.as_str();
    let prefix = format!("{originator}/");
    assert!(user_agent.starts_with(&prefix));
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
    let expected_ua = get_chaos_user_agent();
    let ua_header = headers
        .get("user-agent")
        .expect("user-agent header missing");
    assert_eq!(ua_header.to_str().unwrap(), expected_ua);
}

#[test]
fn test_invalid_suffix_is_sanitized() {
    let prefix = "chaos_cli_rs/0.0.0";
    let suffix = "bad\rsuffix";

    assert_eq!(
        sanitize_user_agent(format!("{prefix} ({suffix})"), prefix),
        "chaos_cli_rs/0.0.0 (bad_suffix)"
    );
}

#[test]
fn test_invalid_suffix_is_sanitized2() {
    let prefix = "chaos_cli_rs/0.0.0";
    let suffix = "bad\0suffix";

    assert_eq!(
        sanitize_user_agent(format!("{prefix} ({suffix})"), prefix),
        "chaos_cli_rs/0.0.0 (bad_suffix)"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn test_macos() {
    use regex_lite::Regex;
    let user_agent = get_chaos_user_agent();
    let originator = regex_lite::escape(originator().value.as_str());
    let re = Regex::new(&format!(
        r"^{originator}/\d+\.\d+\.\d+ \(Mac OS \d+\.\d+\.\d+; (x86_64|arm64)\) (\S+)$"
    ))
    .unwrap();
    assert!(re.is_match(&user_agent));
}
