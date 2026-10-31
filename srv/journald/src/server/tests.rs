use std::time::Duration;

use super::JournalServerConfig;
use super::run_sqlite_journal_server;
use crate::JournalRpcClient;

#[tokio::test(flavor = "multi_thread")]
async fn displaced_server_exits_without_unlinking_its_replacement() {
    let home = tempfile::tempdir().expect("temporary directory");
    let config = JournalServerConfig::from_chaos_home(home.path());
    let socket = config.socket_path.clone();
    let server = tokio::spawn(run_sqlite_journal_server(config));

    let client = JournalRpcClient::new(socket.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.hello("test").await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("journald answers");

    std::fs::remove_file(&socket).expect("unlink journald socket");
    let _replacement = std::os::unix::net::UnixListener::bind(&socket).expect("bind replacement");

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("displaced journald exits")
        .expect("join journald")
        .expect("journald shuts down cleanly");
    assert!(socket.exists(), "replacement socket must survive");
}
