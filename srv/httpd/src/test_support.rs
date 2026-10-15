use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use chaos_kern::ChaosAuth;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::test_support::{
    auth_manager_from_auth_with_home, process_table_with_models_provider_and_home,
};
use rama::futures::stream;
use rama::http::Body;
use tokio::sync::Semaphore;

use crate::{ServerState, monitor, sessions::SessionManager};

pub(crate) async fn server_state(
    body_limit: usize,
    retained_sessions: bool,
) -> (tempfile::TempDir, Arc<ServerState>) {
    let home = tempfile::tempdir().unwrap();
    chaos_kern::test_support::bootstrap_sqlite(home.path()).unwrap();
    let config = ConfigBuilder::default()
        .chaos_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .build()
        .await
        .unwrap();
    let auth = ChaosAuth::from_api_key("test");
    let table = process_table_with_models_provider_and_home(
        auth.clone(),
        config.model_provider.clone(),
        home.path().to_path_buf(),
    );
    let state = Arc::new(ServerState {
        config: Arc::new(config),
        process_table: Arc::new(table),
        auth_manager: auth_manager_from_auth_with_home(auth, home.path().to_path_buf()),
        bearer_token: Arc::from("test-token"),
        recovery_token: None,
        semaphore: Arc::new(Semaphore::new(1)),
        max_concurrent: 1,
        timeout: Duration::from_secs(1),
        body_limit,
        monitor: monitor::MonitorState::new(),
        sessions: retained_sessions
            .then(|| Arc::new(SessionManager::new(4, Duration::from_secs(60)))),
    });
    (home, state)
}

pub(crate) fn counting_body(polls: &Arc<AtomicUsize>) -> Body {
    let polls = polls.clone();
    Body::from_stream(stream::poll_fn(move |_| {
        polls.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Some(Ok::<_, Infallible>("1234")))
    }))
}
