use super::*;

#[tokio::test]
async fn postgres_archive_excludes_writers_and_preserves_scope_and_tombstone() {
    let Some(url) = postgres_test_url() else {
        eprintln!("skipping PostgreSQL archive validation; TEST_DATABASE_URL is not set");
        return;
    };
    let pool = chaos_proc::open_runtime_db_postgres_url(&url)
        .await
        .unwrap();
    let client = JournalClient::postgres_pool(pool.clone());
    let id = ProcessId::new();
    let cwd = std::env::temp_dir().join(id.to_string());
    client
        .create_process(CreateProcessInput {
            process_id: id,
            parent: None,
            source: SessionSource::Acp,
            cwd: cwd.clone(),
            created_at: jiff::Timestamp::now(),
            title: None,
            model_provider: Some("archive-fixture".into()),
            cli_version: None,
        })
        .await
        .unwrap();
    let request = crate::ArchiveIdleProcessRequest {
        process_id: id,
        source: SessionSource::Acp,
        cwd,
        model_provider: "archive-fixture".into(),
    };
    let owner = "archive-writer".to_owned();
    let lease = client
        .acquire_lease(id, owner.clone(), 60_000)
        .await
        .unwrap();
    assert!(client.archive_idle_process(request.clone()).await.is_err());
    client
        .release_lease(id, owner, lease.lease_token)
        .await
        .unwrap();
    assert!(
        client
            .archive_idle_process(crate::ArchiveIdleProcessRequest {
                model_provider: "foreign".into(),
                ..request.clone()
            })
            .await
            .is_err()
    );
    client.archive_idle_process(request.clone()).await.unwrap();
    let timestamp = client
        .get_process(id)
        .await
        .unwrap()
        .unwrap()
        .archived_at
        .unwrap();
    let reopened = JournalClient::postgres_pool(pool);
    reopened.archive_idle_process(request).await.unwrap();
    assert_eq!(
        reopened.get_process(id).await.unwrap().unwrap().archived_at,
        Some(timestamp)
    );
    assert!(
        !reopened
            .list_processes(Some(false))
            .await
            .unwrap()
            .iter()
            .any(|record| record.process_id == id)
    );
}
