use super::hello_is_compatible;
use crate::HelloResponse;
use crate::rama_http::PROTOCOL_VERSION;
use crate::rama_http::SERVER_NAME;
use crate::rama_http::SERVER_VERSION;

fn compatible_hello() -> HelloResponse {
    HelloResponse {
        server_name: SERVER_NAME.to_string(),
        protocol_version: PROTOCOL_VERSION,
        server_version: SERVER_VERSION.to_string(),
        backend: "sqlite".to_string(),
    }
}

#[test]
fn compatibility_requires_exact_server_identity_and_protocol() {
    assert!(hello_is_compatible(&compatible_hello()));

    let mut legacy_journal = compatible_hello();
    legacy_journal.protocol_version = 4;
    assert!(!hello_is_compatible(&legacy_journal));

    let mut stale_version = compatible_hello();
    stale_version.server_version = "47.0.0".to_string();
    assert!(!hello_is_compatible(&stale_version));

    let mut newer_protocol = compatible_hello();
    newer_protocol.protocol_version += 1;
    assert!(!hello_is_compatible(&newer_protocol));

    let mut wrong_server = compatible_hello();
    wrong_server.server_name = "chaos-journald".to_string();
    assert!(!hello_is_compatible(&wrong_server));
}

#[test]
fn legacy_hello_without_server_version_is_incompatible() {
    let hello: HelloResponse = serde_json::from_value(serde_json::json!({
        "server_name": SERVER_NAME,
        "protocol_version": PROTOCOL_VERSION,
        "backend": "sqlite"
    }))
    .unwrap_or_else(|err| panic!("deserialize legacy hello: {err}"));

    assert!(hello.server_version.is_empty());
    assert!(!hello_is_compatible(&hello));
}
