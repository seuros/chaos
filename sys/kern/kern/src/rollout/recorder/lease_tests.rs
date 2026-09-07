use super::*;
use chaos_journald::JournalRpcServer;
use chaos_journald::SqliteJournalStore;
use rama::http::server::HttpServer;
use rama::rt::Executor;
use tokio_util::task::AbortOnDropHandle;

struct TestJournal {
    store: Arc<SqliteJournalStore>,
    client: JournalClient,
    socket: PathBuf,
    _server: AbortOnDropHandle<()>,
    _dir: tempfile::TempDir,
}

impl TestJournal {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("journal.sock");
        let store = Arc::new(
            SqliteJournalStore::open(&dir.path().join("journal.sqlite"))
                .await
                .unwrap(),
        );
        let service = JournalRpcServer::new(Arc::clone(&store), "sqlite").http_service();
        let path = socket.clone();
        let server = AbortOnDropHandle::new(tokio::spawn(async move {
            HttpServer::new_http1(Executor::default())
                .listen_unix(&path, service)
                .await
                .unwrap();
        }));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        Self {
            store,
            client: JournalClient::rpc(JournalRpcClient::new(socket.clone())),
            socket,
            _server: server,
            _dir: dir,
        }
    }

    fn config(&self) -> PendingJournalConfig {
        PendingJournalConfig {
            process_id: ProcessId::new(),
            source: SessionSource::Cli,
            cwd: self._dir.path().to_path_buf(),
            created_at: Timestamp::now(),
            model_provider: "test".into(),
            cli_version: "test".into(),
            owner_id: "owner".into(),
            mode: JournalSinkMode::Resume,
        }
    }

    async fn writer(&self) -> ActiveJournalWriter {
        ActiveJournalWriter::connect_existing(self.client.clone(), &self.config())
            .await
            .unwrap()
    }

    async fn expire(&self, process_id: ProcessId) {
        sqlx::query("UPDATE process_leases SET expires_at = 0 WHERE process_id = ?")
            .bind(process_id.to_string())
            .execute(self.store.pool())
            .await
            .unwrap();
    }
}

fn item(text: &str) -> RolloutItem {
    RolloutItem::Compacted(chaos_ipc::protocol::CompactedItem {
        message: text.into(),
        replacement_history: None,
    })
}

fn assert_item(actual: &RolloutItem, expected: &RolloutItem) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap(),
    );
}

#[tokio::test]
async fn expired_idle_lease_is_reacquired_without_fencing() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    let old_token = writer.lease_token.clone();
    journal.expire(writer.process_id).await;
    writer.last_lease_refresh = Instant::now() - JOURNAL_LEASE_TTL;

    writer.ensure_lease().await.unwrap();
    assert!(!writer.fenced);
    assert!(writer.lease_confirmed);
    assert_ne!(writer.lease_token, old_token);
    writer
        .append_items(&[item("after recovery")])
        .await
        .unwrap();
    assert_eq!(writer.next_seq, 1);
}

#[tokio::test]
async fn expiry_during_append_retains_batch_until_recovery() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    journal.expire(writer.process_id).await;
    let batch = vec![item("pending")];
    assert!(writer.append_items(&batch).await.is_err());
    assert!(!writer.fenced);
    assert!(!writer.lease_confirmed);
    assert_eq!(writer.pending_items.len(), 1);
    assert_item(&writer.pending_items[0], &batch[0]);

    writer.flush_pending_items().await.unwrap();
    assert!(writer.pending_items.is_empty());
    let loaded = journal
        .client
        .load_journal(writer.process_id)
        .await
        .unwrap();
    assert_eq!(loaded.next_seq, 1);
    assert_item(&loaded.items[0].item, &batch[0]);
}

#[tokio::test]
async fn recovery_reconciles_a_lost_response_before_appending_new_items() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    let committed = item("response was lost");
    journal
        .client
        .append_batch(JournalAppendBatchInput {
            process_id: writer.process_id,
            owner_id: writer.owner_id.clone(),
            lease_token: writer.lease_token.clone(),
            expected_next_seq: 0,
            items: vec![JournalEntry {
                seq: 0,
                recorded_at: Timestamp::now(),
                item: committed.clone(),
            }],
        })
        .await
        .unwrap();
    writer.defer_items(&[committed.clone(), item("newly queued")]);
    journal.expire(writer.process_id).await;
    writer.last_lease_refresh = Instant::now() - JOURNAL_LEASE_TTL;

    writer.flush_pending_items().await.unwrap();
    let loaded = journal
        .client
        .load_journal(writer.process_id)
        .await
        .unwrap();
    assert_eq!(loaded.next_seq, 2);
    assert_item(&loaded.items[0].item, &committed);
    assert_item(&loaded.items[1].item, &item("newly queued"));
    assert!(writer.pending_items.is_empty());
}

#[tokio::test]
async fn recovery_never_steals_a_live_lease_or_skips_foreign_history() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    journal.expire(writer.process_id).await;
    writer.needs_reacquire = true;
    writer.lease_confirmed = false;
    let other = journal
        .client
        .acquire_lease(
            writer.process_id,
            "other".into(),
            JOURNAL_LEASE_TTL.as_millis() as u64,
        )
        .await
        .unwrap();
    let error = writer.ensure_lease().await.unwrap_err();
    assert!(error.contains("other"));
    assert!(writer.fenced);
    journal
        .client
        .heartbeat_lease(
            writer.process_id,
            "other".into(),
            other.lease_token,
            JOURNAL_LEASE_TTL.as_millis() as u64,
        )
        .await
        .unwrap();

    let mut stale = journal.writer().await;
    journal
        .client
        .append_batch(JournalAppendBatchInput {
            process_id: stale.process_id,
            owner_id: stale.owner_id.clone(),
            lease_token: stale.lease_token.clone(),
            expected_next_seq: 0,
            items: vec![JournalEntry {
                seq: 0,
                recorded_at: Timestamp::now(),
                item: item("foreign history"),
            }],
        })
        .await
        .unwrap();
    journal.expire(stale.process_id).await;
    stale.last_lease_refresh = Instant::now() - JOURNAL_LEASE_TTL;
    assert!(stale.ensure_lease().await.unwrap_err().contains("changed"));
    assert!(stale.fenced);
    assert_eq!(stale.next_seq, 0);
}

#[tokio::test]
async fn writer_actor_survives_outage_and_conflict() {
    let journal = TestJournal::new().await;
    let mut active = journal.writer().await;
    let process_id = active.process_id;
    active.last_lease_refresh = Instant::now() - JOURNAL_LEASE_REFRESH_INTERVAL;
    let offline_socket = journal.socket.with_extension("offline");
    tokio::fs::rename(&journal.socket, &offline_socket)
        .await
        .unwrap();
    let mut sink = JournalSink::pending(journal.config());
    sink.state = JournalSinkState::Active(active);
    sink.breaker = AsyncCircuitBreaker::new(
        "lease-test",
        1,
        Duration::from_secs(60),
        Duration::from_millis(10),
        1,
    );
    let (tx, rx) = mpsc::unbounded_channel();
    let (status, mut observed) = watch::channel(JournalWriterStatus::Ready);
    let actor = AbortOnDropHandle::new(tokio::spawn(rollout_writer(
        true,
        rx,
        None,
        PathBuf::from("/tmp"),
        None,
        None,
        "test".into(),
        false,
        sink,
        Arc::new(Mutex::new(None)),
        status,
    )));
    tx.send(RolloutCmd::AddItems(vec![item("during outage")]))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while *observed.borrow_and_update() != JournalWriterStatus::Recovering {
            observed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let (ack, done) = oneshot::channel();
    tx.send(RolloutCmd::Flush { ack }).unwrap();
    done.await.unwrap();
    assert!(!actor.is_finished(), "an outage must not stop the writer");

    tokio::fs::rename(&offline_socket, &journal.socket)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (ack, done) = oneshot::channel();
    tx.send(RolloutCmd::Confirm { ack }).unwrap();
    done.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while *observed.borrow_and_update() != JournalWriterStatus::Ready {
            observed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let loaded = journal.client.load_journal(process_id).await.unwrap();
    assert_eq!(loaded.next_seq, 1);
    assert_item(&loaded.items[0].item, &item("during outage"));

    journal.expire(process_id).await;
    journal
        .client
        .acquire_lease(process_id, "other".into(), 30_000)
        .await
        .unwrap();
    tx.send(RolloutCmd::AddItems(vec![item("after takeover")]))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while *observed.borrow_and_update() != JournalWriterStatus::Fenced {
            observed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let (ack, done) = oneshot::channel();
    tx.send(RolloutCmd::Confirm { ack }).unwrap();
    assert!(
        done.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("fenced")
    );
    assert!(
        !actor.is_finished(),
        "fencing must not stop the writer actor"
    );
    let (ack, done) = oneshot::channel();
    tx.send(RolloutCmd::Shutdown { ack }).unwrap();
    done.await.unwrap();
    actor.await.unwrap().unwrap();
}

#[tokio::test]
async fn concurrent_recovery_claims_have_one_winner() {
    let journal = TestJournal::new().await;
    let writer = journal.writer().await;
    journal.expire(writer.process_id).await;
    let (first, second) = tokio::join!(
        journal
            .client
            .acquire_lease(writer.process_id, "first".into(), 30_000),
        journal
            .client
            .acquire_lease(writer.process_id, "second".into(), 30_000),
    );
    let error = match (first, second) {
        (Ok(_), Err(error)) | (Err(error), Ok(_)) => error,
        other => panic!("expected exactly one lease owner: {other:?}"),
    };
    assert!(matches!(error, JournalClientError::Remote(payload)
        if payload.code == JournalErrorCode::LeaseConflict));
}

#[tokio::test(start_paused = true)]
async fn heartbeat_timeout_keeps_writer_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("stalled.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let _server = AbortOnDropHandle::new(tokio::spawn(async move {
        let _connection = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    }));
    let mut writer = ActiveJournalWriter {
        client: JournalClient::rpc(JournalRpcClient::new(socket)),
        process_id: ProcessId::new(),
        owner_id: "owner".into(),
        lease_token: "lease".into(),
        next_seq: 0,
        last_lease_refresh: Instant::now() - JOURNAL_LEASE_REFRESH_INTERVAL,
        pending_items: vec![item("pending")],
        fenced: false,
        lease_confirmed: true,
        needs_reacquire: false,
    };
    assert!(
        writer
            .ensure_lease()
            .await
            .unwrap_err()
            .contains("timed out")
    );
    assert!(!writer.fenced);
    assert!(!writer.lease_confirmed);
    assert_eq!(writer.pending_items.len(), 1);
}
