use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn confirmed_archive_survives_stale_metadata_updates() {
    let home = unique_temp_dir();
    let runtime = StateRuntime::init(home.clone(), "test-provider".into())
        .await
        .unwrap();
    let id = ProcessId::new();
    let stale = test_process_metadata(&home, id, home.clone());
    runtime.upsert_process(&stale).await.unwrap();
    let archived_at = jiff::Timestamp::now();
    runtime
        .archive_process_metadata(id, archived_at)
        .await
        .unwrap();
    runtime.upsert_process(&stale).await.unwrap();
    assert_eq!(
        runtime
            .get_process(id)
            .await
            .unwrap()
            .unwrap()
            .archived_at
            .map(jiff::Timestamp::as_second),
        Some(archived_at.as_second()),
    );
    assert!(
        runtime
            .list_processes(10, None, SortKey::UpdatedAt, &[], None, false, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}
