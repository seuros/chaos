use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_appends_reserve_the_writer_before_reading_fenced_state() {
    let temp = tempdir().unwrap();
    let store = std::sync::Arc::new(
        SqliteJournalStore::open(&temp.path().join("writers.sqlite"))
            .await
            .unwrap(),
    );
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut writers = Vec::new();
    for _ in 0..8 {
        let id = ProcessId::new();
        store
            .create_process(CreateProcessInput {
                process_id: id,
                parent: None,
                source: SessionSource::Acp,
                cwd: temp.path().to_owned(),
                created_at: jiff::Timestamp::now(),
                title: None,
                model_provider: Some("fixture".into()),
                cli_version: None,
            })
            .await
            .unwrap();
        let owner = id.to_string();
        let lease = store
            .acquire_lease(&id, &owner, Duration::from_secs(60))
            .await
            .unwrap();
        let store = store.clone();
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .append_batch(AppendBatchInput {
                    process_id: id,
                    owner_id: owner,
                    lease_token: lease.lease_token,
                    expected_next_seq: 0,
                    items: vec![JournalEntry {
                        seq: 0,
                        recorded_at: jiff::Timestamp::now(),
                        item: message("user", "isolated"),
                    }],
                })
                .await
        }));
    }
    for writer in writers {
        assert_eq!(writer.await.unwrap().unwrap().next_seq, 1);
    }
}

#[tokio::test]
async fn archive_is_scoped_fenced_idempotent_and_persistent() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("archive.sqlite");
    let store = SqliteJournalStore::open(&path).await.unwrap();
    let id = ProcessId::new();
    store
        .create_process(CreateProcessInput {
            process_id: id,
            parent: None,
            source: SessionSource::Acp,
            cwd: temp.path().to_owned(),
            created_at: jiff::Timestamp::now(),
            title: None,
            model_provider: Some("fixture".into()),
            cli_version: None,
        })
        .await
        .unwrap();
    let request = crate::ArchiveIdleProcessRequest {
        process_id: id,
        source: SessionSource::Acp,
        cwd: temp.path().to_owned(),
        model_provider: "fixture".into(),
    };
    for foreign in [
        crate::ArchiveIdleProcessRequest {
            source: SessionSource::Cli,
            ..request.clone()
        },
        crate::ArchiveIdleProcessRequest {
            cwd: temp.path().join("foreign"),
            ..request.clone()
        },
        crate::ArchiveIdleProcessRequest {
            model_provider: "foreign".into(),
            ..request.clone()
        },
    ] {
        assert!(store.archive_idle_process(foreign).await.is_err());
    }
    let owner = "writer".to_owned();
    let lease = store
        .acquire_lease(&id, &owner, Duration::from_secs(60))
        .await
        .unwrap();
    assert!(matches!(
        store.archive_idle_process(request.clone()).await,
        Err(JournalError::LeaseConflict { .. })
    ));
    assert!(
        store
            .get_process(&id)
            .await
            .unwrap()
            .unwrap()
            .archived_at
            .is_none()
    );
    store
        .release_lease(&id, &owner, &lease.lease_token)
        .await
        .unwrap();
    store.archive_idle_process(request.clone()).await.unwrap();
    let timestamp = store
        .get_process(&id)
        .await
        .unwrap()
        .unwrap()
        .archived_at
        .unwrap();
    store.archive_idle_process(request.clone()).await.unwrap();
    assert_eq!(
        store.get_process(&id).await.unwrap().unwrap().archived_at,
        Some(timestamp)
    );
    assert!(store.list_processes(Some(false)).await.unwrap().is_empty());
    drop(store);
    let reopened = SqliteJournalStore::open(&path).await.unwrap();
    assert_eq!(
        reopened
            .get_process(&id)
            .await
            .unwrap()
            .unwrap()
            .archived_at,
        Some(timestamp)
    );
    reopened
        .archive_idle_process(crate::ArchiveIdleProcessRequest {
            process_id: ProcessId::new(),
            ..request
        })
        .await
        .unwrap();
}
