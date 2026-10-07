use super::*;
use crate::{MemoryOptions, MemorySource, MemoryStore};

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn postgres_preview_source_handles_and_concurrent_reinforcement() {
    let fixture = Fixture::ready().await;
    let scope = RecallScope::Project("project".into());
    let sources = vec![MemorySource::File {
        path: "/tmp/source.md".into(),
        fragment: None,
    }];
    fixture
        .store
        .index_memory(
            doc(scope.clone(), "id"),
            MemoryOptions {
                sources: sources.clone(),
                automatic: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .store
        .index(doc(scope.clone(), "private"))
        .await
        .unwrap();
    let mut automatic = filter(scope.clone());
    automatic.automatic_only = true;
    let results = fixture
        .store
        .lexical_search("RecallService", &automatic)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].handle.options.sources, sources);
    let vector_results = fixture
        .store
        .search(&SearchRequest {
            filter: automatic,
            query_vec: vector(),
        })
        .await
        .unwrap();
    assert_eq!(vector_results.len(), 1);
    assert_eq!(vector_results[0].id, "id");
    let receipt = receipt(&fixture, &scope).await;
    assert_eq!(
        fixture
            .store
            .recalled_memory("owner", "generation", &receipt.id, &scope, "id")
            .await
            .unwrap()
            .options
            .charge,
        0.5
    );
    let (a, b) = tokio::join!(
        fixture
            .store
            .commit_use("owner", "generation", &receipt.id, &scope, "id"),
        fixture
            .store
            .commit_use("owner", "generation", &receipt.id, &scope, "id"),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.applied, b.applied);
    assert_eq!(a.charge, 0.52);
    assert_eq!(a.charge, b.charge);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn postgres_receipts_reject_wrong_owner_generation_scope_expiry_and_revision() {
    let fixture = Fixture::ready().await;
    let scope = RecallScope::Session("session".into());
    fixture.store.index(doc(scope.clone(), "id")).await.unwrap();
    let first = receipt(&fixture, &scope).await;
    let other = RecallScope::Session("other".into());
    for (owner, generation, scope) in [
        ("other", "generation", &scope),
        ("owner", "other", &scope),
        ("owner", "generation", &other),
    ] {
        assert!(
            fixture
                .store
                .commit_use(owner, generation, &first.id, scope, "id")
                .await
                .is_err()
        );
        assert!(
            fixture
                .store
                .recalled_memory(owner, generation, &first.id, scope, "id")
                .await
                .is_err()
        );
    }
    fixture
        .store
        .index(doc(scope.clone(), "unselected"))
        .await
        .unwrap();
    assert!(
        fixture
            .store
            .commit_use("owner", "generation", &first.id, &scope, "unselected")
            .await
            .is_err()
    );
    sqlx::query("UPDATE recall_receipts SET expires_at = 0 WHERE id = $1")
        .bind(&first.id)
        .execute(&fixture.pool())
        .await
        .unwrap();
    assert!(
        fixture
            .store
            .commit_use("owner", "generation", &first.id, &scope, "id")
            .await
            .is_err()
    );
    assert!(
        fixture
            .store
            .recalled_memory("owner", "generation", &first.id, &scope, "id")
            .await
            .is_err()
    );
    for delete in [false, true] {
        let receipt = receipt(&fixture, &scope).await;
        if delete {
            fixture.store.delete(&scope, "id").await.unwrap();
        }
        fixture.store.index(doc(scope.clone(), "id")).await.unwrap();
        assert!(
            fixture
                .store
                .commit_use("owner", "generation", &receipt.id, &scope, "id")
                .await
                .is_err()
        );
        assert!(
            fixture
                .store
                .recalled_memory("owner", "generation", &receipt.id, &scope, "id")
                .await
                .is_err()
        );
    }
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn postgres_independent_receipts_do_not_lose_updates_and_charge_is_capped() {
    let fixture = Fixture::ready().await;
    let scope = RecallScope::Project("project".into());
    fixture.store.index(doc(scope.clone(), "id")).await.unwrap();
    let selection = selections(&fixture, &scope).await;
    let a = fixture
        .store
        .issue_receipt("owner-a", "generation", &selection)
        .await
        .unwrap();
    let b = fixture
        .store
        .issue_receipt("owner-b", "generation", &selection)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        fixture
            .store
            .commit_use("owner-a", "generation", &a.id, &scope, "id"),
        fixture
            .store
            .commit_use("owner-b", "generation", &b.id, &scope, "id"),
    );
    assert!(a.unwrap().applied);
    assert!(b.unwrap().applied);
    let current = fixture
        .store
        .lexical_search("RecallService", &filter(scope.clone()))
        .await
        .unwrap();
    assert!((current[0].handle.options.charge - 0.54).abs() < 1e-9);
    fixture
        .store
        .index_memory(
            doc(scope.clone(), "id"),
            MemoryOptions {
                charge: 0.99,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let receipt = receipt(&fixture, &scope).await;
    assert_eq!(
        fixture
            .store
            .commit_use("owner", "generation", &receipt.id, &scope, "id")
            .await
            .unwrap()
            .charge,
        1.0
    );
    fixture.store.migrate().await.unwrap();
    assert_eq!(
        fixture
            .store
            .recalled_memory("owner", "generation", &receipt.id, &scope, "id")
            .await
            .unwrap()
            .options
            .charge,
        1.0
    );
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn postgres_receipt_issuance_rolls_back_partial_selections() {
    let fixture = Fixture::ready().await;
    let scope = RecallScope::Project("project".into());
    fixture
        .store
        .index_batch(vec![doc(scope.clone(), "a"), doc(scope.clone(), "z")])
        .await
        .unwrap();
    let stale = selections(&fixture, &scope).await;
    fixture.store.index(doc(scope.clone(), "z")).await.unwrap();
    assert!(matches!(
        fixture
            .store
            .issue_receipt("owner", "generation", &stale)
            .await,
        Err(RecallError::InvalidReceipt)
    ));
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM recall_receipts), (SELECT count(*) FROM recall_selections)",
    )
    .fetch_one(&fixture.pool())
    .await
    .unwrap();
    assert_eq!(counts, (0, 0));
    receipt(&fixture, &scope).await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and pgvector"]
async fn postgres_receipts_are_bounded_per_owner() {
    let fixture = Fixture::ready().await;
    let scope = RecallScope::Session("session".into());
    fixture.store.index(doc(scope.clone(), "id")).await.unwrap();
    let selected = selections(&fixture, &scope).await;
    let oldest = receipt(&fixture, &scope).await;
    let other = fixture
        .store
        .issue_receipt("other", "generation", &selected)
        .await
        .unwrap();
    let mut latest = oldest.clone();
    for _ in 0..100 {
        latest = fixture
            .store
            .issue_receipt("owner", "generation", &selected)
            .await
            .unwrap();
    }
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM recall_receipts WHERE owner = 'owner'),
                (SELECT count(*) FROM recall_selections)",
    )
    .fetch_one(&fixture.pool())
    .await
    .unwrap();
    assert_eq!(counts, (100, 101));
    assert!(matches!(
        fixture
            .store
            .recalled_memory("owner", "generation", &oldest.id, &scope, "id")
            .await,
        Err(RecallError::InvalidReceipt)
    ));
    for (owner, receipt) in [("owner", latest), ("other", other)] {
        assert!(
            fixture
                .store
                .recalled_memory(owner, "generation", &receipt.id, &scope, "id")
                .await
                .is_ok()
        );
    }
    fixture.cleanup().await;
}
