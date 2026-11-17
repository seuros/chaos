use super::*;
use chaos_mcp_runtime::GuestError;

#[test]
fn classifies_guest_errors_through_anyhow_context() {
    let transport = anyhow::Error::from(GuestError::Transport(std::io::Error::other(
        "connection dropped",
    )));
    assert!(is_transient(&transport));
    assert!(is_transient(&transport.context("MCP call failed")));

    let server = anyhow::Error::from(GuestError::Server {
        code: -32601,
        message: "method not found".into(),
        data: None,
    })
    .context("MCP call failed");
    assert!(!is_transient(&server));
    assert!(!is_transient(&anyhow::anyhow!("unclassified failure")));
}

#[tokio::test]
async fn transport_failures_open_breaker() {
    let server = "regression-transport-failures-open-breaker";
    for _ in 0..5 {
        let result = with_circuit_breaker(server, || async {
            Err::<(), _>(
                anyhow::Error::from(GuestError::Transport(std::io::Error::other(
                    "connection dropped",
                )))
                .context("MCP call failed"),
            )
        })
        .await;
        assert!(result.is_err());
    }
    let called = std::cell::Cell::new(false);
    assert!(
        with_circuit_breaker(server, || async {
            called.set(true);
            Ok(())
        })
        .await
        .is_err()
    );
    assert!(!called.get(), "open breaker must reject the operation");
}
