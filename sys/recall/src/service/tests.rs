use std::sync::atomic::{AtomicUsize, Ordering};

use chaos_vfs::{ChaosVfs, MountConfig};
use tokio::sync::Barrier;

use super::*;
use crate::store::EMBEDDING_DIM;

mod evaluation;

#[derive(Clone, Copy, Default)]
enum Behavior {
    #[default]
    Success,
    Error,
    Hang,
    Panic,
}

#[derive(Default)]
struct FakeStore {
    semantic: Behavior,
    lexical: Behavior,
    semantic_results: Vec<SearchResult>,
    lexical_results: Vec<SearchResult>,
    barrier: Option<Arc<Barrier>>,
    calls: AtomicUsize,
    writes: AtomicUsize,
    dropped: AtomicUsize,
}

struct DropCounter<'a>(&'a AtomicUsize);

impl Drop for DropCounter<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl FakeStore {
    async fn retrieve(
        &self,
        behavior: Behavior,
        results: &[SearchResult],
    ) -> Result<Vec<SearchResult>, RecallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _guard = DropCounter(&self.dropped);
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        match behavior {
            Behavior::Success => Ok(results.to_vec()),
            Behavior::Error => Err(RecallError::Backend(anyhow::anyhow!(
                "test backend failure"
            ))),
            Behavior::Hang => std::future::pending().await,
            Behavior::Panic => panic!("test executor panic"),
        }
    }
}

impl RecallStore for FakeStore {
    async fn index(&self, doc: RecallDoc) -> Result<(), RecallError> {
        doc.validate()?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn index_batch(&self, docs: Vec<RecallDoc>) -> Result<(), RecallError> {
        for doc in docs {
            self.index(doc).await?;
        }
        Ok(())
    }

    async fn search(&self, req: &SearchRequest) -> Result<Vec<SearchResult>, RecallError> {
        assert!(req.filter.limit <= 250);
        assert_eq!(req.filter.model_fingerprint, "test-model");
        self.retrieve(self.semantic, &self.semantic_results).await
    }

    async fn lexical_search(
        &self,
        _query: &str,
        filter: &RetrievalFilter,
    ) -> Result<Vec<SearchResult>, RecallError> {
        assert!(filter.limit <= 250);
        self.retrieve(self.lexical, &self.lexical_results).await
    }

    async fn delete(&self, _scope: &RecallScope, _id: &str) -> Result<(), RecallError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeEmbedder {
    fail: bool,
    invalid: bool,
    calls: AtomicUsize,
}

impl EmbeddingProvider for FakeEmbedder {
    fn fingerprint(&self) -> &str {
        "test-model"
    }

    async fn embed(
        &self,
        _text: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<f32>, RecallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if cancellation.is_cancelled() {
            return Err(RecallError::Cancelled);
        }
        if self.fail {
            return Err(RecallError::Backend(anyhow::anyhow!(
                "test embedding failure"
            )));
        }
        let mut vector = vec![0.0; EMBEDDING_DIM];
        if !self.invalid {
            vector[0] = 2.0;
        }
        Ok(vector)
    }
}

fn result(id: &str) -> SearchResult {
    SearchResult {
        scope: RecallScope::Project("chaos".into()),
        id: id.into(),
        score: 0.9,
        content: format!("content {id}"),
        metadata: serde_json::json!({"source": "fixture"}),
        handle: crate::MemoryHandle::default(),
    }
}

fn service(store: FakeStore, embedder: FakeEmbedder) -> RecallService<FakeStore, FakeEmbedder> {
    RecallService::new(store, embedder, Duration::from_secs(1))
}

fn options() -> SearchOptions {
    SearchOptions::new(RecallScope::Project("chaos".into()))
}

#[tokio::test]
async fn branches_overlap_and_fusion_deduplicates_with_provenance() {
    let store = FakeStore {
        semantic_results: vec![result("semantic"), result("shared")],
        lexical_results: vec![result("lexical"), result("shared")],
        barrier: Some(Arc::new(Barrier::new(2))),
        ..Default::default()
    };
    let service = service(store, FakeEmbedder::default());
    let response = service
        .search("recall", options(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(service.store.calls.load(Ordering::SeqCst), 2);
    assert_eq!(response.matches.len(), 3);
    assert_eq!(response.matches[0].id, "shared");
    assert_eq!(
        response.matches[0].methods,
        [RetrievalMethod::Semantic, RetrievalMethod::Lexical]
    );
    assert_eq!(response.matches[0].metadata["source"], "fixture");
    assert!(response.warnings.is_empty());
}

#[tokio::test]
async fn each_single_branch_failure_preserves_the_other_branch() {
    for (semantic, lexical, failed) in [
        (
            Behavior::Error,
            Behavior::Success,
            RetrievalMethod::Semantic,
        ),
        (Behavior::Success, Behavior::Error, RetrievalMethod::Lexical),
    ] {
        let service = service(
            FakeStore {
                semantic,
                lexical,
                semantic_results: vec![result("hit")],
                lexical_results: vec![result("hit")],
                ..Default::default()
            },
            FakeEmbedder::default(),
        );
        let response = service
            .search("recall", options(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.warnings.len(), 1);
        assert_eq!(response.warnings[0].method, failed);
        assert_eq!(response.warnings[0].failure_kind, "backend");
    }
}

#[tokio::test]
async fn embedding_failure_degrades_to_lexical() {
    let service = service(
        FakeStore {
            lexical_results: vec![result("exact-name")],
            ..Default::default()
        },
        FakeEmbedder {
            fail: true,
            ..Default::default()
        },
    );
    let response = service
        .search("exact-name", options(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.matches[0].id, "exact-name");
    assert_eq!(response.warnings[0].method, RetrievalMethod::Semantic);
    assert_eq!(service.store.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn both_failures_are_an_error_but_empty_successes_are_not() {
    let service = service(
        FakeStore {
            semantic: Behavior::Error,
            lexical: Behavior::Error,
            ..Default::default()
        },
        FakeEmbedder::default(),
    );
    assert!(matches!(
        service
            .search("recall", options(), CancellationToken::new())
            .await,
        Err(RecallError::RetrievalFailed { .. })
    ));
    let empty = super::RecallService::new(
        FakeStore::default(),
        FakeEmbedder::default(),
        Duration::from_secs(1),
    );
    let response = empty
        .search("recall", options(), CancellationToken::new())
        .await
        .unwrap();
    assert!(response.matches.is_empty());
    assert!(response.warnings.is_empty());
}

#[tokio::test]
async fn failed_embedding_never_writes_a_document() {
    for embedder in [
        FakeEmbedder {
            fail: true,
            ..Default::default()
        },
        FakeEmbedder {
            invalid: true,
            ..Default::default()
        },
    ] {
        let service = service(FakeStore::default(), embedder);
        assert!(
            service
                .store(
                    RecallScope::Global,
                    "id".into(),
                    "content".into(),
                    serde_json::json!({}),
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
        assert_eq!(service.store.writes.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn store_embeds_text_once() {
    let service = service(FakeStore::default(), FakeEmbedder::default());
    service
        .store(
            RecallScope::Global,
            "id".into(),
            "content".into(),
            serde_json::json!({}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(service.embedder.calls.load(Ordering::SeqCst), 1);
    assert_eq!(service.store.writes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_requests_do_not_launch_work() {
    let service = service(FakeStore::default(), FakeEmbedder::default());
    for (query, limit) in [("", 10), ("  ", 10), ("query", 0), ("query", 51)] {
        let mut options = options();
        options.limit = limit;
        assert!(matches!(
            service
                .search(query, options, CancellationToken::new())
                .await,
            Err(RecallError::InvalidInput(_))
        ));
    }
    assert_eq!(service.store.calls.load(Ordering::SeqCst), 0);
    assert_eq!(service.embedder.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_documents_are_rejected_before_embedding() {
    let service = service(FakeStore::default(), FakeEmbedder::default());
    for (scope, id, content) in [
        (RecallScope::Project(" ".into()), "id", "content"),
        (RecallScope::Session(" ".into()), "id", "content"),
        (RecallScope::Global, " ", "content"),
        (RecallScope::Global, "id", " "),
    ] {
        assert!(matches!(
            service
                .store(
                    scope,
                    id.into(),
                    content.into(),
                    serde_json::json!({}),
                    CancellationToken::new()
                )
                .await,
            Err(RecallError::InvalidInput(_))
        ));
    }
    assert_eq!(service.embedder.calls.load(Ordering::SeqCst), 0);
    assert_eq!(service.store.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn limit_is_enforced_after_fusion() {
    let service = service(
        FakeStore {
            semantic_results: (0..20).map(|id| result(&format!("{id:02}"))).collect(),
            ..Default::default()
        },
        FakeEmbedder::default(),
    );
    let mut options = options();
    options.limit = 3;
    let response = service
        .search("query", options, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.matches.len(), 3);
    assert_eq!(response.matches[0].id, "00");
}

#[tokio::test]
async fn cancellation_drains_active_branches() {
    let service = service(
        FakeStore {
            semantic: Behavior::Hang,
            lexical: Behavior::Hang,
            ..Default::default()
        },
        FakeEmbedder::default(),
    );
    let cancellation = CancellationToken::new();
    let task = service.search("query", options(), cancellation.clone());
    let cancel = async {
        while service.store.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        cancellation.cancel();
    };
    let (result, _) = tokio::join!(task, cancel);
    assert!(matches!(result, Err(RecallError::Cancelled)));
    assert_eq!(service.store.dropped.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn already_cancelled_request_launches_nothing() {
    let service = service(FakeStore::default(), FakeEmbedder::default());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        service.search("query", options(), cancellation).await,
        Err(RecallError::Cancelled)
    ));
    assert_eq!(service.store.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn already_cancelled_store_does_not_embed_or_write() {
    let service = service(FakeStore::default(), FakeEmbedder::default());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        service
            .store(
                RecallScope::Global,
                "id".into(),
                "content".into(),
                serde_json::json!({}),
                cancellation
            )
            .await,
        Err(RecallError::Cancelled)
    ));
    assert_eq!(service.embedder.calls.load(Ordering::SeqCst), 0);
    assert_eq!(service.store.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn postgres_model_errors_are_reported_before_database_work() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://postgres@127.0.0.1:1/unreachable")
        .unwrap();
    let result = initialize_with_loader(
        Some(Vfs::Postgres(pool)),
        RecallConfig::new("/missing-model"),
        CancellationToken::new(),
        |_, _, _| async {
            Err::<FakeEmbedder, _>(RecallError::ModelLoad(anyhow::anyhow!(
                "missing model artifacts"
            )))
        },
    )
    .await;
    assert!(matches!(result, Err(RecallError::ModelLoad(_))));
}

#[tokio::test]
async fn deadline_aborts_outstanding_actions() {
    let service = RecallService::new(
        FakeStore {
            semantic: Behavior::Hang,
            lexical: Behavior::Hang,
            ..Default::default()
        },
        FakeEmbedder::default(),
        Duration::from_millis(20),
    );
    assert!(matches!(
        service
            .search("query", options(), CancellationToken::new())
            .await,
        Err(RecallError::Timeout)
    ));
    tokio::task::yield_now().await;
    assert_eq!(service.store.dropped.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn executor_panics_are_not_degraded_results() {
    // Panic/backtrace formatting must not race the service's wall-clock
    // deadline on a busy runner. This test owns panic propagation, not timing.
    let service = service(
        FakeStore {
            semantic: Behavior::Panic,
            lexical: Behavior::Hang,
            ..Default::default()
        },
        FakeEmbedder::default(),
    );
    let result = service
        .search("query", options(), CancellationToken::new())
        .await;
    assert!(
        matches!(result, Err(RecallError::Workflow(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn disabled_backends_do_not_call_loader_even_with_bad_config() {
    let temp = tempfile::tempdir().unwrap();
    let vfs = ChaosVfs::from_config(MountConfig::sqlite_home(temp.path()))
        .await
        .unwrap();
    for mount in [None, Some(vfs.pool())] {
        let mut config = RecallConfig::new("/missing-model");
        config.embedding_concurrency = 0;
        let result =
            initialize_with_loader(mount, config, CancellationToken::new(), |_, _, _| async {
                panic!("disabled recall must not load a model");
                #[allow(unreachable_code)]
                Ok(FakeEmbedder::default())
            })
            .await
            .unwrap();
        assert!(matches!(result, RecallState::Disabled));
    }
}

#[tokio::test]
async fn missing_local_artifacts_produce_actionable_errors() {
    let temp = tempfile::tempdir().unwrap();
    let loaded = LocalEmbedder::load(temp.path().to_owned(), 1, CancellationToken::new()).await;
    match loaded {
        Err(RecallError::ModelLoad(error)) => {
            assert!(format!("{error:#}").contains("tokenizer.json"))
        }
        _ => panic!("missing artifacts must not be accepted"),
    }
}

#[test]
fn fusion_has_stable_ties_and_scoped_identity() {
    let mut other_scope = result("same");
    other_scope.scope = RecallScope::Session("session".into());
    let response = fuse(
        Ok(vec![
            result("same"),
            result("duplicate"),
            result("duplicate"),
        ]),
        Ok(vec![other_scope]),
        50,
    )
    .unwrap();
    assert_eq!(response.matches.len(), 3);
    assert_eq!(
        response.matches[0].scope,
        RecallScope::Project("chaos".into())
    );
    assert_eq!(
        response.matches[1].scope,
        RecallScope::Session("session".into())
    );
    assert_eq!(response.matches[2].methods.len(), 1);
}

#[test]
fn dedup_counts_each_branch_once_without_merging_identical_text() {
    let shared = result("shared");
    let response = fuse(
        Ok(vec![shared.clone(), shared.clone()]),
        Ok(vec![shared.clone(), shared.clone()]),
        10,
    )
    .unwrap();
    assert_eq!(response.matches.len(), 1);
    assert_eq!(
        response.matches[0].methods,
        [RetrievalMethod::Semantic, RetrievalMethod::Lexical]
    );
    assert!((response.matches[0].rank_score - 2.0 / 61.0).abs() < 1e-12);

    let mut different_identity = shared.clone();
    different_identity.id = "different".into();
    let response = fuse(Ok(vec![shared, different_identity]), Ok(vec![]), 10).unwrap();
    assert_eq!(
        response.matches.len(),
        2,
        "identical content is not a shared identity"
    );
}

#[test]
fn missing_workflow_branches_preserve_diagnostic_names() {
    let mut state = RequestState::default();
    assert!(matches!(state.take_branches(),
        Err(RecallError::Workflow(message)) if message == "missing semantic branch"));
    *state.branch(RetrievalMethod::Semantic) = Some(Ok(vec![]));
    assert!(matches!(state.take_branches(),
        Err(RecallError::Workflow(message)) if message == "missing lexical branch"));
}

#[test]
fn invalid_vectors_are_rejected_and_valid_vectors_are_normalized() {
    assert!(matches!(
        normalize_embedding(vec![1.0]),
        Err(RecallError::DimMismatch { .. })
    ));
    assert!(normalize_embedding(vec![0.0; EMBEDDING_DIM]).is_err());
    assert!(normalize_embedding(vec![f32::NAN; EMBEDDING_DIM]).is_err());
    assert!(normalize_embedding(vec![f32::INFINITY; EMBEDDING_DIM]).is_err());
    let vector = normalize_embedding(vec![f32::MAX; EMBEDDING_DIM]).unwrap();
    let norm: f32 = vector.iter().map(|value| value * value).sum();
    assert!((norm - 1.0).abs() < 1e-5);
}
