use super::direct_journal_client_for_vfs;
use super::processes_page_from_db;
use super::*;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_journald::JournalClient;
use sqlx::postgres::PgPoolOptions;
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn background_append_reconciliation_requires_the_same_sequence_and_payload() {
    let item =
        RolloutItem::BackgroundTask(chaos_ipc::background_tasks::TaskJournalEvent::WakePolicy {
            policy: chaos_ipc::background_tasks::WakePolicy::Interrupted,
        });
    let journal = LoadedJournal {
        process_id: ProcessId::new(),
        parent: None,
        next_seq: 8,
        items: vec![JournalEntry {
            seq: 7,
            recorded_at: Timestamp::now(),
            item: item.clone(),
        }],
    };
    assert!(batch_matches_journal(
        &journal,
        7,
        std::slice::from_ref(&item)
    ));
    assert!(!batch_matches_journal(&journal, 8, &[item]));
    let other =
        RolloutItem::BackgroundTask(chaos_ipc::background_tasks::TaskJournalEvent::WakePolicy {
            policy: chaos_ipc::background_tasks::WakePolicy::Closed,
        });
    assert!(!batch_matches_journal(&journal, 7, &[other]));
}

#[tokio::test]
async fn background_confirm_does_not_report_a_deferred_writer_as_durable() {
    let mut sink = JournalSink::pending(PendingJournalConfig {
        process_id: ProcessId::new(),
        source: SessionSource::Cli,
        cwd: PathBuf::from("/tmp"),
        created_at: Timestamp::now(),
        model_provider: "test".into(),
        cli_version: "test".into(),
        owner_id: "test".into(),
        mode: JournalSinkMode::Create,
    });
    sink.state = JournalSinkState::Disabled;
    let expires_at: Timestamp = "2026-09-06T18:00:30Z".parse().unwrap();
    sink.last_error = Some(
        chaos_journald::JournalError::LeaseConflict {
            process_id: ProcessId::new(),
            current_owner_id: "other-writer".into(),
            expires_at,
        }
        .to_string(),
    );
    let resume_error = sink
        .failure("cannot claim journal writer for resumed process")
        .to_string();
    assert!(resume_error.contains("other-writer"));
    assert!(resume_error.contains(&expires_at.to_string()));
    assert!(resume_error.contains("close the other session instance"));
    let (tx, rx) = mpsc::unbounded_channel();
    let writer = tokio::spawn(rollout_writer(
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
        watch::channel(JournalWriterStatus::Ready).0,
    ));
    let (ack, done) = oneshot::channel();
    tx.send(RolloutCmd::Flush { ack }).unwrap();
    done.await.unwrap();
    let (ack, done) = oneshot::channel();
    tx.send(RolloutCmd::Confirm { ack }).unwrap();
    let error = done.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("journal has no confirmed writer lease"));
    assert!(error.contains("other-writer"));
    drop(tx);
    writer.await.unwrap().unwrap();
}

#[tokio::test]
async fn background_fenced_writer_cannot_reacquire_a_lease() {
    let mut writer = ActiveJournalWriter {
        client: JournalClient::rpc(JournalRpcClient::new(PathBuf::from(
            "/nonexistent-journald.sock",
        ))),
        process_id: ProcessId::new(),
        owner_id: "owner".into(),
        lease_token: "lease".into(),
        next_seq: 0,
        last_lease_refresh: Instant::now(),
        pending_items: Vec::new(),
        fenced: true,
        lease_confirmed: false,
        needs_reacquire: false,
    };
    assert!(writer.ensure_lease().await.unwrap_err().contains("fenced"));
}

#[tokio::test]
async fn postgres_vfs_selects_direct_journal_client() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://localhost/chaos")
        .expect("create lazy PostgreSQL pool");
    let client = direct_journal_client_for_vfs(chaos_vfs::Vfs::Postgres(pool))
        .expect("PostgreSQL should have a direct journal client");
    assert!(matches!(client, JournalClient::Postgres(_)));
}

#[test]
fn converts_runtime_db_page_without_losing_resume_metadata() {
    let process_id = ProcessId::from_string("00000000-0000-0000-0000-000000000123").unwrap();
    let next_id = Uuid::parse_str("00000000-0000-0000-0000-000000000456").unwrap();
    let created_at: jiff::Timestamp = "2026-09-04T10:00:00Z".parse().unwrap();
    let updated_at: jiff::Timestamp = "2026-09-04T11:00:00Z".parse().unwrap();
    let db_page = chaos_proc::ProcessesPage {
        items: vec![chaos_proc::ProcessMetadata {
            id: process_id,
            created_at,
            updated_at,
            source: "cli".to_string(),
            agent_nickname: None,
            agent_role: None,
            model_provider: "openai".to_string(),
            cwd: PathBuf::from("/tmp/project"),
            cli_version: "47.2.0".to_string(),
            title: "Fix resume".to_string(),
            sandbox_policy: "workspace-write".to_string(),
            approval_mode: "interactive".to_string(),
            tokens_used: 42,
            first_user_message: None,
            archived_at: None,
            git_sha: Some("abc123".to_string()),
            git_branch: Some("main".to_string()),
            git_origin_url: Some("https://example.com/chaos.git".to_string()),
        }],
        next_anchor: Some(chaos_proc::Anchor {
            ts: updated_at,
            id: next_id,
        }),
        num_scanned_rows: 2,
    };

    let page = processes_page_from_db(db_page);

    assert_eq!(page.num_scanned_records, 2);
    assert!(!page.reached_scan_limit);
    assert_eq!(
        page.next_cursor.as_ref().map(super::Cursor::id),
        Some(next_id)
    );
    assert_eq!(
        page.next_cursor
            .as_ref()
            .map(|cursor| cursor.ts().unix_timestamp()),
        Some(updated_at.as_second())
    );
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    let expected_cwd = PathBuf::from("/tmp/project");
    assert_eq!(item.process_id, Some(process_id));
    assert_eq!(item.first_user_message.as_deref(), Some("Fix resume"));
    assert_eq!(item.model_provider.as_deref(), Some("openai"));
    assert_eq!(item.tokens_used, Some(42));
    assert_eq!(item.cwd.as_deref(), Some(expected_cwd.as_path()));
    assert_eq!(item.source, Some(SessionSource::Cli));
    assert_eq!(item.git_branch.as_deref(), Some("main"));
}
