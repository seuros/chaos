use super::*;
use crate::MemorySource;
use crate::test_support::{Embedder, Store};

struct Fixture {
    store: Store,
    service: RecallService<Store, Embedder>,
    scope: RecallScope,
}

impl Fixture {
    fn new() -> Self {
        let store = Store::default();
        let service = RecallService::new(store.clone(), Embedder, Duration::from_secs(5));
        Self {
            store,
            service,
            scope: RecallScope::Project("project".into()),
        }
    }

    async fn store(&self, id: &str, options: MemoryOptions) {
        self.store_content(id, "memory", options).await;
    }

    async fn store_content(&self, id: &str, content: &str, options: MemoryOptions) {
        self.service
            .store_memory(
                self.scope.clone(),
                id.into(),
                content.into(),
                serde_json::json!({"provenance": "verbatim"}),
                options,
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }

    async fn recall(&self) -> RecallResponse {
        let mut response = self
            .service
            .search(
                "memory",
                SearchOptions::new(self.scope.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        self.service
            .attach_receipt("owner", &mut response)
            .await
            .unwrap();
        response
    }
}

#[tokio::test(start_paused = true)]
async fn timestamps_follow_content_and_new_uses_not_reads_or_replays() {
    let fixture = Fixture::new();
    fixture.store("id", MemoryOptions::default()).await;
    let original = fixture.recall().await.matches[0].handle.timestamps;
    assert_eq!(original.created_at, original.updated_at);
    assert_eq!(original.last_used_at, None);

    tokio::time::advance(Duration::from_secs(1)).await;
    fixture.store("id", MemoryOptions::default()).await;
    assert_eq!(
        fixture.recall().await.matches[0].handle.timestamps,
        original
    );
    fixture
        .store_content("id", "changed memory", MemoryOptions::default())
        .await;
    let changed = fixture.recall().await;
    let timestamps = changed.matches[0].handle.timestamps;
    assert_eq!(timestamps.created_at, original.created_at);
    assert!(timestamps.updated_at > original.updated_at);
    assert_eq!(timestamps.last_used_at, None);
    let receipt = changed.receipt.unwrap().id;

    tokio::time::advance(Duration::from_secs(1)).await;
    let used = fixture
        .service
        .use_memory("owner", &receipt, &fixture.scope, "id")
        .await
        .unwrap();
    assert!(used.applied);
    let after_use = fixture.recall().await.matches[0].handle.timestamps;
    assert_eq!(after_use.created_at, timestamps.created_at);
    assert_eq!(after_use.updated_at, timestamps.updated_at);
    assert!(after_use.last_used_at.unwrap() > timestamps.updated_at);

    tokio::time::advance(Duration::from_secs(1)).await;
    let opened = fixture
        .service
        .recalled_memory("owner", &receipt, &fixture.scope, "id")
        .await
        .unwrap();
    assert_eq!(opened.timestamps, after_use);
    let json = serde_json::to_value(&opened).unwrap();
    assert!(json["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        serde_json::from_value::<MemoryHandle>(json)
            .unwrap()
            .timestamps,
        after_use
    );
    let replay = fixture
        .service
        .use_memory("owner", &receipt, &fixture.scope, "id")
        .await
        .unwrap();
    assert!(!replay.applied);
    assert_eq!(
        fixture.recall().await.matches[0].handle.timestamps,
        after_use
    );

    fixture
        .store_content(
            "id",
            "changed memory",
            MemoryOptions {
                charge: 1.0,
                ..Default::default()
            },
        )
        .await;
    let capped = fixture.recall().await;
    assert_eq!(capped.matches[0].handle.timestamps, after_use);
    let outcome = fixture
        .service
        .use_memory("owner", &capped.receipt.unwrap().id, &fixture.scope, "id")
        .await
        .unwrap();
    assert!(outcome.applied);
    assert_eq!(outcome.charge, 1.0);
    assert!(
        fixture.recall().await.matches[0]
            .handle
            .timestamps
            .last_used_at
            > after_use.last_used_at
    );

    fixture
        .service
        .delete(&fixture.scope, "id", CancellationToken::new())
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    fixture.store("id", MemoryOptions::default()).await;
    let recreated = fixture.recall().await.matches[0].handle.timestamps;
    assert!(recreated.created_at > original.created_at);
    assert_eq!(recreated.updated_at, recreated.created_at);
    assert_eq!(recreated.last_used_at, None);
}

#[tokio::test]
async fn search_and_source_resolution_do_not_reinforce_but_concurrent_use_applies_once() {
    let fixture = Fixture::new();
    let sources = vec![MemorySource::File {
        path: "/tmp/source.md".into(),
        fragment: Some("decisions".into()),
    }];
    fixture
        .store(
            "id",
            MemoryOptions {
                sources: sources.clone(),
                ..Default::default()
            },
        )
        .await;
    let response = fixture.recall().await;
    let receipt = response.receipt.unwrap().id;
    assert_eq!(
        response.matches[0].metadata,
        serde_json::json!({"provenance": "verbatim"})
    );
    let handle = fixture
        .service
        .recalled_memory("owner", &receipt, &fixture.scope, "id")
        .await
        .unwrap();
    assert_eq!(handle.options.sources, sources);
    assert_eq!(handle.options.charge, 0.5);
    assert_eq!(fixture.recall().await.matches[0].handle.options.charge, 0.5);
    let (first, second) = tokio::join!(
        fixture
            .service
            .use_memory("owner", &receipt, &fixture.scope, "id"),
        fixture
            .service
            .use_memory("owner", &receipt, &fixture.scope, "id"),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first.applied, second.applied);
    assert_eq!(first.charge, 0.52);
    assert_eq!(first.charge, second.charge);
    assert_eq!(
        fixture.recall().await.matches[0].handle.revision,
        handle.revision
    );
}

#[tokio::test]
async fn charge_is_capped_and_replays_return_the_original_outcome() {
    let fixture = Fixture::new();
    fixture
        .store(
            "id",
            MemoryOptions {
                charge: 0.99,
                ..Default::default()
            },
        )
        .await;
    let receipt = fixture.recall().await.receipt.unwrap().id;
    let first = fixture
        .service
        .use_memory("owner", &receipt, &fixture.scope, "id")
        .await
        .unwrap();
    assert!(first.applied);
    assert_eq!(first.charge, 1.0);
    let next = fixture.recall().await.receipt.unwrap().id;
    assert_eq!(
        fixture
            .service
            .use_memory("owner", &next, &fixture.scope, "id")
            .await
            .unwrap()
            .charge,
        1.0
    );
    let replay = fixture
        .service
        .use_memory("owner", &receipt, &fixture.scope, "id")
        .await
        .unwrap();
    assert!(!replay.applied);
    assert_eq!(replay.charge, first.charge);
}

#[tokio::test]
async fn receipts_are_bound_to_owner_selection_scope_and_service_generation() {
    let fixture = Fixture::new();
    fixture.store("id", MemoryOptions::default()).await;
    let receipt = fixture.recall().await.receipt.unwrap().id;
    fixture.store("unselected", MemoryOptions::default()).await;
    let other = RecallScope::Project("other".into());
    for (owner, scope, id) in [
        ("other-owner", &fixture.scope, "id"),
        ("owner", &other, "id"),
        ("owner", &fixture.scope, "unselected"),
        ("owner", &RecallScope::Global, "id"),
    ] {
        assert!(matches!(
            fixture.service.use_memory(owner, &receipt, scope, id).await,
            Err(RecallError::InvalidReceipt)
        ));
    }
    assert!(
        fixture
            .service
            .recalled_memory("other-owner", &receipt, &fixture.scope, "id")
            .await
            .is_err()
    );
    let restarted = RecallService::new(fixture.store.clone(), Embedder, Duration::from_secs(5));
    assert!(
        restarted
            .use_memory("owner", &receipt, &fixture.scope, "id")
            .await
            .is_err()
    );
    assert!(
        restarted
            .recalled_memory("owner", &receipt, &fixture.scope, "id")
            .await
            .is_err()
    );
    assert_eq!(fixture.recall().await.matches[0].handle.options.charge, 0.5);
}

#[tokio::test]
async fn replacement_and_delete_recreate_invalidate_prior_receipts() {
    let fixture = Fixture::new();
    for delete in [false, true] {
        fixture.store("id", MemoryOptions::default()).await;
        let response = fixture.recall().await;
        let receipt = response.receipt.unwrap().id;
        if delete {
            fixture
                .service
                .delete(&fixture.scope, "id", CancellationToken::new())
                .await
                .unwrap();
            assert!(
                fixture
                    .service
                    .recalled_memory("owner", &receipt, &fixture.scope, "id")
                    .await
                    .is_err()
            );
        }
        fixture.store("id", MemoryOptions::default()).await;
        assert!(
            fixture
                .service
                .use_memory("owner", &receipt, &fixture.scope, "id")
                .await
                .is_err()
        );
        assert!(
            fixture
                .service
                .recalled_memory("owner", &receipt, &fixture.scope, "id")
                .await
                .is_err()
        );
        assert_ne!(
            fixture.recall().await.matches[0].handle.revision,
            response.matches[0].handle.revision
        );
    }
}

#[tokio::test(start_paused = true)]
async fn expired_receipts_cannot_open_or_reinforce() {
    let fixture = Fixture::new();
    fixture.store("id", MemoryOptions::default()).await;
    let receipt = fixture.recall().await.receipt.unwrap().id;
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert!(
        fixture
            .service
            .use_memory("owner", &receipt, &fixture.scope, "id")
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .recalled_memory("owner", &receipt, &fixture.scope, "id")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn automatic_filter_is_opt_in_and_charge_only_breaks_equal_ranks() {
    let fixture = Fixture::new();
    fixture.store("private", MemoryOptions::default()).await;
    fixture
        .store(
            "low",
            MemoryOptions {
                automatic: true,
                charge: 0.1,
                ..Default::default()
            },
        )
        .await;
    fixture
        .store(
            "high",
            MemoryOptions {
                automatic: true,
                charge: 0.9,
                ..Default::default()
            },
        )
        .await;
    let options = SearchOptions {
        automatic_only: true,
        ..SearchOptions::new(fixture.scope.clone())
    };
    let response = fixture
        .service
        .search("memory", options, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.matches.len(), 2);
    assert!(
        response
            .matches
            .iter()
            .all(|item| item.handle.options.automatic)
    );
    assert!(response.matches.iter().all(|item| item.id != "private"));
    let mut low = response.matches[1].clone();
    let mut high = response.matches[0].clone();
    low.handle.options.charge = 0.1;
    high.handle.options.charge = 0.9;
    let results = [low, high].map(|item| SearchResult {
        scope: item.scope,
        id: item.id,
        score: 1.0,
        content: item.content,
        metadata: item.metadata,
        handle: item.handle,
    });
    let tied = fuse(
        Ok(vec![results[0].clone(), results[1].clone()]),
        Ok(vec![results[1].clone(), results[0].clone()]),
        2,
    )
    .unwrap();
    assert_eq!(tied.matches[0].handle.options.charge, 0.9);
}

#[test]
fn memory_options_and_sources_are_strict_and_bounded() {
    for value in [
        serde_json::json!({"automatic": true, "unknown": "x"}),
        serde_json::json!({"created_at": "2026-10-07T00:00:00Z"}),
        serde_json::json!({"updated_at": "2026-10-07T00:00:00Z"}),
        serde_json::json!({"last_used_at": "2026-10-07T00:00:00Z"}),
        serde_json::json!({"sources": [{"kind": "url", "path": "/tmp/x"}]}),
        serde_json::json!({"sources": [{"kind": "file", "path": "/tmp/x", "command": "sh"}]}),
    ] {
        assert!(serde_json::from_value::<MemoryOptions>(value).is_err());
    }
    for charge in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        assert!(
            MemoryOptions {
                charge,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    for path in ["relative", "/tmp/../secret", "/tmp/\0secret"] {
        assert!(
            MemorySource::File {
                path: path.into(),
                fragment: None
            }
            .validate()
            .is_err()
        );
    }
    assert!(!MemoryOptions::default().automatic);
}
