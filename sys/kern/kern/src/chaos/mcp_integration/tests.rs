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

#[tokio::test]
async fn late_success_preserves_open_timestamp_and_allows_reset() {
    let server = "regression-late-success-preserves-open-timestamp";
    let (started_tx, started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let late_success = with_circuit_breaker(server, || async {
        started_tx.send(()).unwrap();
        finish_rx.await.unwrap();
        Ok(())
    });
    let failures = async {
        started_rx.await.unwrap();
        for _ in 0..5 {
            assert!(
                with_circuit_breaker(server, || async {
                    Err::<(), _>(GuestError::Timeout(Duration::from_secs(1)).into())
                })
                .await
                .is_err()
            );
        }
        let breaker = mcp_circuit_breaker(server);
        let opened_at = {
            let guard = breaker.lock().unwrap();
            assert!(guard.breaker.is_open());
            guard.opened_at.unwrap()
        };
        finish_tx.send(()).unwrap();
        opened_at
    };
    let (result, opened_at) = tokio::join!(late_success, failures);
    result.unwrap();

    let breaker = mcp_circuit_breaker(server);
    {
        let mut guard = breaker.lock().unwrap();
        assert!(guard.breaker.is_open());
        assert_eq!(guard.opened_at, Some(opened_at));
        guard.opened_at = Some(Instant::now() - HALF_OPEN_TIMEOUT);
    }
    with_circuit_breaker(server, || async { Ok(()) })
        .await
        .unwrap();
    let guard = breaker.lock().unwrap();
    assert!(!guard.breaker.is_open());
    assert!(guard.opened_at.is_none());
}
