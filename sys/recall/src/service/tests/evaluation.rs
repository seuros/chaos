use super::*;
use crate::pg::tests::Fixture;

#[derive(Deserialize)]
struct Evaluation {
    documents: Vec<Document>,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Document {
    id: String,
    content: String,
}

#[derive(Deserialize)]
struct Query {
    query: String,
    relevant: Vec<String>,
}

#[derive(Default)]
struct Metrics {
    recall: f64,
    mrr: f64,
}

impl Metrics {
    fn observe(&mut self, ids: &[String], relevant: &[String]) {
        self.recall += ids
            .iter()
            .take(5)
            .filter(|id| relevant.contains(id))
            .count() as f64
            / relevant.len() as f64;
        self.mrr += ids
            .iter()
            .take(5)
            .position(|id| relevant.contains(id))
            .map_or(0.0, |rank| 1.0 / (rank + 1) as f64);
    }
}

#[tokio::test]
#[ignore = "requires RECALL_MODEL_DIR with local potion-base-8M artifacts"]
async fn real_model_smoke_test() {
    let directory = std::env::var("RECALL_MODEL_DIR").expect("set RECALL_MODEL_DIR");
    let embedder = LocalEmbedder::load(directory.into(), 2, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(embedder.fingerprint().len(), 64);
    let same = embedder
        .embed("the kernel mounts PostgreSQL", CancellationToken::new())
        .await
        .unwrap();
    let again = embedder
        .embed("the kernel mounts PostgreSQL", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(same, again);
    assert_eq!(same.len(), EMBEDDING_DIM);
    let norm: f32 = same.iter().map(|value| value * value).sum();
    assert!((norm - 1.0).abs() < 1e-5);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL, pgvector, and RECALL_MODEL_DIR"]
async fn real_model_hybrid_quality_and_service_round_trip() {
    let directory = std::env::var("RECALL_MODEL_DIR").expect("set RECALL_MODEL_DIR");
    let fixture = Fixture::new().await;
    let state = RecallState::from_mount(
        Some(Vfs::Postgres(fixture.pool())),
        RecallConfig::new(directory),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let RecallState::Ready(service) = state else {
        panic!("PostgreSQL must enable recall");
    };
    let evaluation: Evaluation =
        serde_json::from_str(include_str!("../../../tests/fixtures/evaluation.json")).unwrap();
    let scope = RecallScope::Project("evaluation".into());
    for doc in &evaluation.documents {
        service
            .store(
                scope.clone(),
                doc.id.clone(),
                doc.content.clone(),
                serde_json::json!({"source": "evaluation.json"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    let mut lexical = Metrics::default();
    let mut semantic = Metrics::default();
    let mut hybrid = Metrics::default();
    for query in &evaluation.queries {
        let filter = RetrievalFilter {
            scope: scope.clone(),
            include_global: false,
            limit: 25,
            model_fingerprint: service.embedder.fingerprint().to_owned(),
            automatic_only: false,
        };
        let query_vec = service
            .embedder
            .embed(&query.query, CancellationToken::new())
            .await
            .unwrap();
        let semantic_results = service
            .store
            .search(&SearchRequest {
                filter: filter.clone(),
                query_vec,
            })
            .await
            .unwrap();
        let lexical_results = service
            .store
            .lexical_search(&query.query, &filter)
            .await
            .unwrap();
        let mut options = SearchOptions::new(scope.clone());
        options.limit = 5;
        let response = service
            .search(&query.query, options, CancellationToken::new())
            .await
            .unwrap();
        assert!(response.warnings.is_empty());
        assert!(
            response
                .matches
                .iter()
                .all(|doc| doc.metadata["source"] == "evaluation.json")
        );
        let semantic_ids: Vec<_> = semantic_results.into_iter().map(|doc| doc.id).collect();
        let lexical_ids: Vec<_> = lexical_results.into_iter().map(|doc| doc.id).collect();
        let hybrid_ids: Vec<_> = response.matches.into_iter().map(|doc| doc.id).collect();
        eprintln!(
            "query={:?} semantic={:?} lexical={:?} hybrid={:?}",
            query.query,
            &semantic_ids[..semantic_ids.len().min(5)],
            &lexical_ids[..lexical_ids.len().min(5)],
            hybrid_ids
        );
        semantic.observe(&semantic_ids, &query.relevant);
        lexical.observe(&lexical_ids, &query.relevant);
        hybrid.observe(&hybrid_ids, &query.relevant);
    }
    let count = evaluation.queries.len() as f64;
    eprintln!(
        "Recall@5 / MRR@5: lexical={:.4}/{:.4}, semantic={:.4}/{:.4}, hybrid={:.4}/{:.4}",
        lexical.recall / count,
        lexical.mrr / count,
        semantic.recall / count,
        semantic.mrr / count,
        hybrid.recall / count,
        hybrid.mrr / count,
    );
    // Clean up even if the quality acceptance gate fails.
    service
        .delete(&scope, "scope", CancellationToken::new())
        .await
        .unwrap();
    let lexical_deleted = service
        .store
        .lexical_search(
            "RecallScope",
            &RetrievalFilter {
                scope,
                include_global: false,
                limit: 5,
                model_fingerprint: service.embedder.fingerprint().into(),
                automatic_only: false,
            },
        )
        .await
        .unwrap();
    assert!(lexical_deleted.is_empty());
    drop(service);
    fixture.cleanup().await;
    assert!(
        hybrid.recall >= semantic.recall.max(lexical.recall),
        "hybrid Recall@5 must not regress below the better single branch"
    );
}
