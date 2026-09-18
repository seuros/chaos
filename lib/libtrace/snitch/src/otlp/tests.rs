use super::*;
use pretty_assertions::assert_eq;
use tokio::runtime::Builder;

#[test]
fn current_tokio_runtime_is_multi_thread_detects_runtime_flavor() {
    assert!(!current_tokio_runtime_is_multi_thread());

    let current_thread_runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    assert_eq!(
        current_thread_runtime.block_on(async { current_tokio_runtime_is_multi_thread() }),
        false
    );

    let multi_thread_runtime = Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    assert_eq!(
        multi_thread_runtime.block_on(async { current_tokio_runtime_is_multi_thread() }),
        true
    );
}
