use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn polling_stops_at_the_deadline_without_panicking() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/deviceauth/token"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1..)
        .mount(&server)
        .await;
    let client = ChaosHttpClient::default_client();
    let started = std::time::Instant::now();
    let Err(error) = poll_for_token(
        &client,
        &server.uri(),
        "auth-id",
        "CODE",
        60,
        Duration::from_millis(300),
    )
    .await
    else {
        panic!("a never-approved device code must not yield a token");
    };
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}
