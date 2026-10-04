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
        replacement_history: Vec::new(),
    })
}

fn assert_item(actual: &RolloutItem, expected: &RolloutItem) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap(),
    );
}

#[tokio::test]
async fn goal_checkpoint_round_trips_through_durable_sqlite_journal() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    let checkpoint = RolloutItem::GoalCheckpoint(chaos_ipc::protocol::GoalCheckpointItem {
        conversation_id: writer.process_id,
        server: "driver".into(),
        endpoint: "stdio:test".into(),
        version: 2,
        snapshot: serde_json::json!({"id":"goal","revision":2,"checks":1,"status":"checking"}),
        verdict: None,
    });
    writer
        .append_items(std::slice::from_ref(&checkpoint))
        .await
        .unwrap();
    let loaded = journal
        .client
        .load_journal(writer.process_id)
        .await
        .unwrap();
    assert_eq!(loaded.items.len(), 1);
    assert_item(&loaded.items[0].item, &checkpoint);
    writer.release_lease().await.unwrap();
}

#[tokio::test]
async fn expired_idle_lease_is_reacquired_without_fencing() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    let warning_at = writer.lease.warning_deadline().unwrap();
    let confirmed_at = warning_at - JOURNAL_LEASE_TTL + Duration::from_secs(5);
    let renewal_at = confirmed_at + JOURNAL_LEASE_REFRESH_INTERVAL;
    assert!(
        !writer
            .lease
            .renewal_due(renewal_at - Duration::from_nanos(1))
    );
    assert!(writer.lease.renewal_due(renewal_at));
    assert!(!writer.lease.at_risk(warning_at - Duration::from_nanos(1)));
    assert!(writer.lease.at_risk(warning_at));
    let old_token = writer.lease_token.clone();
    journal.expire(writer.process_id).await;
    writer
        .lease
        .set_confirmed_at(Instant::now() - JOURNAL_LEASE_TTL);

    writer.ensure_lease().await.unwrap();
    assert!(!writer.lease.fenced());
    assert!(writer.lease.confirmed());
    assert!(!writer.lease.renewal_due(Instant::now()));
    assert!(writer.lease.warning_deadline().unwrap() > Instant::now());
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
    assert!(!writer.lease.fenced());
    assert!(!writer.lease.confirmed());
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
    writer
        .lease
        .set_confirmed_at(Instant::now() - JOURNAL_LEASE_TTL);

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
    writer.lease.apply(WriterLeaseEvent::Expire);
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
    assert!(writer.lease.fenced());
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
    stale
        .lease
        .set_confirmed_at(Instant::now() - JOURNAL_LEASE_TTL);
    assert!(stale.ensure_lease().await.unwrap_err().contains("changed"));
    assert!(stale.lease.fenced());
    assert_eq!(stale.next_seq, 0);
}

#[tokio::test]
async fn writer_actor_survives_outage_and_conflict() {
    let journal = TestJournal::new().await;
    let mut active = journal.writer().await;
    let process_id = active.process_id;
    active
        .lease
        .set_confirmed_at(Instant::now() - JOURNAL_LEASE_REFRESH_INTERVAL);
    let offline_socket = journal.socket.with_extension("offline");
    tokio::fs::rename(&journal.socket, &offline_socket)
        .await
        .unwrap();
    let mut sink = JournalSink::pending(journal.config());
    sink.activate(active);
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
        watch::channel(None).0,
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
    done.await.unwrap().unwrap();
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
    std::assert_matches!(error, JournalClientError::Remote(payload)
        if payload.code == JournalErrorCode::LeaseConflict);
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
        pending_items: vec![item("pending")],
        lease: Lease::new(Instant::now() - JOURNAL_LEASE_REFRESH_INTERVAL),
    };
    let warning_at = writer.lease.warning_deadline();
    assert!(
        writer
            .ensure_lease()
            .await
            .unwrap_err()
            .contains("timed out")
    );
    assert!(!writer.lease.fenced());
    assert!(!writer.lease.confirmed());
    assert_eq!(writer.lease.warning_deadline(), warning_at);
    assert!(writer.lease.renewal_due(Instant::now()));
    assert_eq!(writer.pending_items.len(), 1);
}

fn recorder(tx: UnboundedSender<RolloutCmd>) -> RolloutRecorder {
    RolloutRecorder {
        tx,
        runtime_db: None,
        event_persistence_mode: EventPersistenceMode::Extended,
        live_rollout_items: Arc::new(Mutex::new(Vec::new())),
        writer_status: watch::channel(JournalWriterStatus::Ready).1,
    }
}

async fn assert_shutdown_releases_lease(client: JournalClient, config: PendingJournalConfig) {
    // Exercise the same writer lifecycle through either SQLite RPC or the
    // direct PostgreSQL client, without consulting the mounted database.
    for scenario in ["normal", "open-breaker", "failed-flush", "dropped-recorder"] {
        let mut config = config.clone();
        config.process_id = ProcessId::new();
        let mut writer = ActiveJournalWriter::connect_existing(client.clone(), &config)
            .await
            .unwrap();
        writer.defer_items(&[item("queued before shutdown")]);
        let mut sink = JournalSink::pending(config.clone());
        if scenario == "open-breaker" {
            let _ = sink.breaker.call(|| async { Err::<(), _>("outage") }).await;
            assert!(sink.breaker.retry_after().is_some());
        }
        if scenario == "failed-flush" {
            // A history conflict can fence a writer while it still owns a lease.
            writer.lease.apply(WriterLeaseEvent::Fence);
        }
        sink.activate(writer);
        let (tx, rx) = mpsc::unbounded_channel();
        let actor = AbortOnDropHandle::new(tokio::spawn(rollout_writer(
            true,
            rx,
            None,
            config.cwd.clone(),
            None,
            None,
            "test".into(),
            false,
            sink,
            watch::channel(None).0,
            watch::channel(JournalWriterStatus::Ready).0,
        )));
        let recorder = recorder(tx);
        if scenario == "dropped-recorder" {
            drop(recorder);
        } else {
            recorder.shutdown().await.unwrap();
        }
        actor.await.unwrap().unwrap();

        let lease = client
            .acquire_lease(config.process_id, "next-writer".into(), 30_000)
            .await
            .unwrap_or_else(|error| panic!("{scenario} left a live lease: {error}"));
        if scenario != "failed-flush" {
            let loaded = client.load_journal(config.process_id).await.unwrap();
            assert_eq!(loaded.next_seq, 1);
            assert_item(&loaded.items[0].item, &item("queued before shutdown"));
        }
        client
            .release_lease(config.process_id, lease.owner_id, lease.lease_token)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn sqlite_shutdown_releases_lease_on_every_exit_path() {
    let journal = TestJournal::new().await;
    assert_shutdown_releases_lease(journal.client.clone(), journal.config()).await;
}

#[tokio::test]
async fn postgres_shutdown_releases_lease_on_every_exit_path() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        tracing::warn!("skipping PostgreSQL lease cleanup; TEST_DATABASE_URL is not set");
        return;
    };
    let pool = chaos_proc::open_runtime_db_postgres_url(&url)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = PendingJournalConfig {
        process_id: ProcessId::new(),
        source: SessionSource::Cli,
        cwd: dir.path().to_path_buf(),
        created_at: Timestamp::now(),
        model_provider: "test".into(),
        cli_version: "test".into(),
        owner_id: "shutdown-test".into(),
        mode: JournalSinkMode::Resume,
    };
    assert_shutdown_releases_lease(JournalClient::postgres_pool(pool), config).await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_waits_for_release_acknowledgement_with_a_deadline() {
    for complete in [true, false] {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let recorder = recorder(tx);
        let shutdown = tokio::spawn(async move { recorder.shutdown().await });
        let Some(RolloutCmd::Shutdown { ack }) = rx.recv().await else {
            panic!("expected shutdown command");
        };
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished(), "lease cleanup is still running");
        if complete {
            ack.send(Ok(())).unwrap();
            shutdown.await.unwrap().unwrap();
        } else {
            tokio::time::advance(JOURNAL_SHUTDOWN_TIMEOUT).await;
            assert_eq!(
                shutdown.await.unwrap().unwrap_err().kind(),
                std::io::ErrorKind::TimedOut
            );
            assert!(ack.is_closed());
        }
    }
}

#[tokio::test]
async fn shutdown_reports_release_failure() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    writer.client = JournalClient::rpc(JournalRpcClient::new(
        journal.socket.with_extension("missing"),
    ));
    let mut sink = JournalSink::pending(journal.config());
    sink.activate(writer);
    let error = sink.shutdown().await.unwrap_err();
    assert!(error.to_string().contains("release_lease failed"));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let recorder = recorder(tx);
    let shutdown = tokio::spawn(async move { recorder.shutdown().await });
    let Some(RolloutCmd::Shutdown { ack }) = rx.recv().await else {
        panic!("expected shutdown command");
    };
    ack.send(Err(error)).unwrap();
    assert!(
        shutdown
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("release_lease failed")
    );
}

#[tokio::test]
async fn failed_resume_releases_lease_acquired_before_loading_journal() {
    let journal = TestJournal::new().await;
    let config = journal.config();
    let writer = ActiveJournalWriter::connect_existing(journal.client.clone(), &config)
        .await
        .unwrap();
    writer.release_lease().await.unwrap();
    sqlx::query(
        "INSERT INTO journal_entries (process_id, seq, recorded_at, item_type, payload_json)
         VALUES (?, 0, 0, 'compacted', 'invalid')",
    )
    .bind(config.process_id.to_string())
    .execute(journal.store.pool())
    .await
    .unwrap();
    let error = ActiveJournalWriter::connect_existing(journal.client.clone(), &config)
        .await
        .expect_err("loading corrupt history should fail");
    assert!(error.to_string().contains("load_journal failed"));
    journal
        .client
        .acquire_lease(config.process_id, "next-writer".into(), 30_000)
        .await
        .expect("failed resume must not leave a live lease");
}

#[tokio::test]
async fn resumed_session_lease_conflict_preserves_type_and_diagnostics() {
    let journal = TestJournal::new().await;
    let writer = journal.writer().await;
    let mut config = journal.config();
    config.process_id = writer.process_id;
    config.owner_id = "second-writer".into();
    let error = ActiveJournalWriter::connect_existing(journal.client.clone(), &config)
        .await
        .expect_err("an active writer must block resume");
    std::assert_matches!(
        error.downcast_ref::<ChaosErr>(),
        Some(ChaosErr::SessionInUse(id)) if *id == writer.process_id
    );
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("LeaseConflict"));
    assert!(diagnostic.contains(&writer.owner_id));
    assert!(diagnostic.contains("holds the lease until"));

    let mut sink = JournalSink::pending(config);
    sink.last_error = Some(error);
    let io_error = sink.failure("cannot claim journal writer for resumed process");
    let mapped = super::super::error::map_session_init_error(
        &anyhow::Error::new(io_error),
        journal._dir.path(),
    );
    std::assert_matches!(mapped, ChaosErr::SessionInUse(id) if id == writer.process_id);
    assert!(!mapped.to_string().contains("ErrorPayload"));

    // Formatting must not change ownership or release the other writer's lease.
    writer
        .client
        .heartbeat_lease(
            writer.process_id,
            writer.owner_id.clone(),
            writer.lease_token.clone(),
            30_000,
        )
        .await
        .unwrap();
    writer.release_lease().await.unwrap();
}

#[tokio::test]
async fn stale_shutdown_does_not_release_another_writers_lease() {
    let journal = TestJournal::new().await;
    let mut writer = journal.writer().await;
    let process_id = writer.process_id;
    journal.expire(process_id).await;
    let lease = journal
        .client
        .acquire_lease(process_id, "next-writer".into(), 30_000)
        .await
        .unwrap();
    writer.lease.apply(WriterLeaseEvent::Fence);
    writer.defer_items(&[item("must not be written")]);
    let mut sink = JournalSink::pending(journal.config());
    sink.activate(writer);
    sink.shutdown().await.unwrap();
    journal
        .client
        .heartbeat_lease(process_id, lease.owner_id, lease.lease_token, 30_000)
        .await
        .expect("stale shutdown must leave the new owner untouched");
    assert_eq!(
        journal
            .client
            .load_journal(process_id)
            .await
            .unwrap()
            .next_seq,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn release_timeout_is_bounded_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("stalled.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let _server = AbortOnDropHandle::new(tokio::spawn(async move {
        let _connection = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    }));
    let writer = ActiveJournalWriter {
        client: JournalClient::rpc(JournalRpcClient::new(socket)),
        process_id: ProcessId::new(),
        owner_id: "owner".into(),
        lease_token: "lease".into(),
        next_seq: 0,
        pending_items: Vec::new(),
        lease: Lease::default(),
    };
    let started = tokio::time::Instant::now();
    assert_eq!(
        writer.release_lease().await.unwrap_err(),
        "release_lease timed out"
    );
    assert_eq!(started.elapsed(), JOURNAL_REQUEST_TIMEOUT);
}
