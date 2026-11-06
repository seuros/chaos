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

#[tokio::test(start_paused = true)]
async fn half_open_admits_a_single_probe_after_cooldown() {
    let server = "half-open-single-probe";
    let breaker = build_mcp_circuit_breaker(server);
    let (started_tx, started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let late_success = guarded(&breaker, server, || async {
        started_tx.send(()).unwrap();
        finish_rx.await.unwrap();
        Ok(())
    });
    let failures = async {
        started_rx.await.unwrap();
        for _ in 0..5 {
            assert!(
                guarded(&breaker, server, || async {
                    Err::<(), _>(GuestError::Timeout(Duration::from_secs(1)).into())
                })
                .await
                .is_err()
            );
        }
        assert!(breaker.is_open());
        finish_tx.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(late_success, failures);
    result.unwrap();
    assert!(
        breaker.is_open(),
        "a success admitted before the trip cannot close the circuit"
    );
    assert_eq!(breaker.retry_after(), Some(HALF_OPEN_TIMEOUT.as_secs_f64()));

    tokio::time::advance(HALF_OPEN_TIMEOUT).await;
    let (probe_started_tx, probe_started_rx) = oneshot::channel();
    let (probe_finish_tx, probe_finish_rx) = oneshot::channel();
    let probe = guarded(&breaker, server, || async {
        probe_started_tx.send(()).unwrap();
        probe_finish_rx.await.unwrap();
        Ok(())
    });
    let competitor = async {
        probe_started_rx.await.unwrap();
        let called = std::cell::Cell::new(false);
        let rejected = guarded(&breaker, server, || async {
            called.set(true);
            Ok(())
        })
        .await;
        assert!(rejected.is_err(), "only one probe runs while half-open");
        assert!(!called.get());
        probe_finish_tx.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(probe, competitor);
    result.unwrap();
    assert!(
        !breaker.is_closed(),
        "one success is short of the threshold"
    );
    guarded(&breaker, server, || async { Ok(()) })
        .await
        .unwrap();
    assert!(breaker.is_closed());
}
