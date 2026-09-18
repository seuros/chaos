use super::*;
use pretty_assertions::assert_eq;
use std::future::pending;
use tokio_test::assert_pending;
use tokio_test::assert_ready;

#[tokio::test]
async fn returns_ok_when_future_completes_first() {
    let token = CancellationToken::new();
    let value = async { 42 };

    let result = value.or_cancel(&token).await;

    assert_eq!(Ok(42), result);
}

#[tokio::test]
async fn returns_err_when_token_cancelled_first() {
    let token = CancellationToken::new();
    let mut task = tokio_test::task::spawn(pending::<i32>().or_cancel(&token));

    assert_pending!(task.poll());
    token.cancel();
    assert!(task.is_woken(), "cancellation must wake the waiting future");
    assert_eq!(Err(CancelErr::Cancelled), assert_ready!(task.poll()));
}

#[tokio::test]
async fn returns_err_when_token_already_cancelled() {
    let token = CancellationToken::new();
    token.cancel();

    let result = pending::<i32>().or_cancel(&token).await;

    assert_eq!(Err(CancelErr::Cancelled), result);
}
