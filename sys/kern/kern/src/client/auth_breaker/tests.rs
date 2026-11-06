use super::*;
use crate::error::ProviderAuthMissingError;
use std::cell::Cell;

fn auth_missing() -> ChaosErr {
    ChaosErr::ProviderAuthMissing(ProviderAuthMissingError {
        provider_id: "test".into(),
        provider_name: "test".into(),
        env_key: None,
        env_key_instructions: None,
        supports_oauth: false,
    })
}

fn rejected() -> ChaosErr {
    ChaosErr::InternalServerError
}

async fn probe(breaker: &AuthBreaker, outcome: Result<(), ChaosErr>) -> Result<(), ChaosErr> {
    let called = Cell::new(false);
    let result = breaker
        .probe(
            || async {
                called.set(true);
                outcome
            },
            rejected,
        )
        .await;
    assert!(called.get(), "a closed breaker must run the probe");
    result
}

async fn assert_rejected(breaker: &AuthBreaker) {
    let called = Cell::new(false);
    let result = breaker
        .probe(
            || async {
                called.set(true);
                Ok(())
            },
            rejected,
        )
        .await;
    std::assert_matches!(result, Err(ChaosErr::InternalServerError));
    assert!(!called.get(), "an open breaker must not probe");
}

#[tokio::test(start_paused = true)]
async fn missing_credentials_trip_then_close_on_auth() {
    let breaker = AuthBreaker::new("test-provider");

    probe(&breaker, Ok(())).await.unwrap();
    std::assert_matches!(
        probe(&breaker, Err(auth_missing())).await,
        Err(ChaosErr::ProviderAuthMissing(_))
    );
    assert_rejected(&breaker).await;

    tokio::time::advance(AUTH_HALF_OPEN_TIMEOUT).await;
    probe(&breaker, Ok(())).await.unwrap();
    probe(&breaker, Ok(())).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn half_open_admits_a_single_probe() {
    let breaker = AuthBreaker::new("test-provider-probe");
    std::assert_matches!(
        probe(&breaker, Err(auth_missing())).await,
        Err(ChaosErr::ProviderAuthMissing(_))
    );
    tokio::time::advance(AUTH_HALF_OPEN_TIMEOUT).await;

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let first = breaker.probe(
        || async {
            started_tx.send(()).unwrap();
            finish_rx.await.unwrap();
            Err::<(), _>(auth_missing())
        },
        rejected,
    );
    let second = async {
        started_rx.await.unwrap();
        assert_rejected(&breaker).await;
        finish_tx.send(()).unwrap();
    };
    let (first, ()) = tokio::join!(first, second);
    std::assert_matches!(first, Err(ChaosErr::ProviderAuthMissing(_)));
    assert_rejected(&breaker).await;
}

#[tokio::test(start_paused = true)]
async fn reset_reopens_probing_after_login() {
    let breaker = AuthBreaker::new("test-provider-reset");
    std::assert_matches!(
        probe(&breaker, Err(auth_missing())).await,
        Err(ChaosErr::ProviderAuthMissing(_))
    );
    assert_rejected(&breaker).await;
    breaker.reset();
    probe(&breaker, Ok(())).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn transient_errors_do_not_trip() {
    let breaker = AuthBreaker::new("test-provider-transient");
    std::assert_matches!(
        probe(&breaker, Err(ChaosErr::InternalServerError)).await,
        Err(ChaosErr::InternalServerError)
    );
    probe(&breaker, Ok(())).await.unwrap();
}
