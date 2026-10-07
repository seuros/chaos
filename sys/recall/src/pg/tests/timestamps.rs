use super::*;
use crate::{MemoryHandle, MemoryOptions, MemoryStore};

async fn handle(fixture: &Fixture, scope: &RecallScope) -> MemoryHandle {
    fixture
        .store
        .lexical_search("RecallService", &filter(scope.clone()))
        .await
        .unwrap()
        .remove(0)
        .handle
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn postgres_timestamps_follow_content_and_new_uses_not_reads_or_replays() {
    let fixture = Fixture::ready().await;
    let scope = RecallScope::Project("project".into());
    let mut document = doc(scope.clone(), "id");
    fixture.store.index(document.clone()).await.unwrap();
    let original = handle(&fixture, &scope).await;
    assert_eq!(
        original.timestamps.created_at,
        original.timestamps.updated_at
    );
    assert_eq!(original.timestamps.last_used_at, None);
    let semantic = fixture
        .store
        .search(&SearchRequest {
            filter: filter(scope.clone()),
            query_vec: vector(),
        })
        .await
        .unwrap();
    assert_eq!(semantic[0].handle.timestamps, original.timestamps);

    document.metadata = serde_json::json!({"changed": true});
    fixture.store.index(document.clone()).await.unwrap();
    let same_content = handle(&fixture, &scope).await;
    assert_eq!(same_content.timestamps, original.timestamps);
    assert_ne!(same_content.revision, original.revision);
    document.content.push_str(" changed");
    fixture.store.index(document.clone()).await.unwrap();
    let changed = handle(&fixture, &scope).await;
    assert_eq!(
        changed.timestamps.created_at,
        original.timestamps.created_at
    );
    assert!(changed.timestamps.updated_at > original.timestamps.updated_at);
    assert_eq!(changed.timestamps.last_used_at, None);

    let first = receipt(&fixture, &scope).await;
    assert_eq!(
        fixture
            .store
            .recalled_memory("owner", "generation", &first.id, &scope, "id")
            .await
            .unwrap()
            .timestamps,
        changed.timestamps
    );
    assert!(
        fixture
            .store
            .commit_use("other", "generation", &first.id, &scope, "id")
            .await
            .is_err()
    );
    assert_eq!(
        handle(&fixture, &scope).await.timestamps,
        changed.timestamps
    );
    assert!(
        fixture
            .store
            .commit_use("owner", "generation", &first.id, &scope, "id")
            .await
            .unwrap()
            .applied
    );
    let used = handle(&fixture, &scope).await.timestamps;
    assert_eq!(used.created_at, changed.timestamps.created_at);
    assert_eq!(used.updated_at, changed.timestamps.updated_at);
    assert!(used.last_used_at.unwrap() > used.updated_at);
    assert!(
        !fixture
            .store
            .commit_use("owner", "generation", &first.id, &scope, "id")
            .await
            .unwrap()
            .applied
    );
    fixture.store.migrate().await.unwrap();
    let opened = fixture
        .store
        .recalled_memory("owner", "generation", &first.id, &scope, "id")
        .await
        .unwrap();
    assert_eq!(opened.timestamps, used);
    assert_eq!(
        serde_json::from_value::<MemoryHandle>(serde_json::to_value(opened).unwrap())
            .unwrap()
            .timestamps,
        used
    );
    fixture
        .store
        .index_memory(
            document,
            MemoryOptions {
                charge: 1.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(handle(&fixture, &scope).await.timestamps, used);
    let next = receipt(&fixture, &scope).await;
    let capped = fixture
        .store
        .commit_use("owner", "generation", &next.id, &scope, "id")
        .await
        .unwrap();
    assert_eq!(capped.charge, 1.0);
    assert!(capped.applied);
    assert!(handle(&fixture, &scope).await.timestamps.last_used_at > used.last_used_at);

    fixture.store.delete(&scope, "id").await.unwrap();
    fixture.store.index(doc(scope.clone(), "id")).await.unwrap();
    let recreated = handle(&fixture, &scope).await.timestamps;
    assert!(recreated.created_at > original.timestamps.created_at);
    assert_eq!(recreated.updated_at, recreated.created_at);
    assert_eq!(recreated.last_used_at, None);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn postgres_timestamps_do_not_change_retrieval_order() {
    let fixture = Fixture::ready().await;
    let scope = RecallScope::Session("session".into());
    fixture.store.index(doc(scope.clone(), "z")).await.unwrap();
    fixture.store.index(doc(scope.clone(), "a")).await.unwrap();
    for results in [
        fixture
            .store
            .lexical_search("RecallService", &filter(scope.clone()))
            .await
            .unwrap(),
        fixture
            .store
            .search(&SearchRequest {
                filter: filter(scope),
                query_vec: vector(),
            })
            .await
            .unwrap(),
    ] {
        assert_eq!(results[0].id, "a");
        assert_eq!(results[1].id, "z");
        assert!(results[0].handle.timestamps.created_at > results[1].handle.timestamps.created_at);
    }
    fixture.cleanup().await;
}
