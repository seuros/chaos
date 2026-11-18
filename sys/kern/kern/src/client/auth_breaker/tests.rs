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
async fn transient_errors_do_not_trip() {
    let breaker = AuthBreaker::new("test-provider-transient");
    std::assert_matches!(
        probe(&breaker, Err(ChaosErr::InternalServerError)).await,
        Err(ChaosErr::InternalServerError)
    );
    probe(&breaker, Ok(())).await.unwrap();
}
