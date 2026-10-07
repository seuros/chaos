use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chaos_synopsis::{
    ActionExecutor, ActionFuture, ActionId, ActionOutcome, Node, Outcome, Runner, Synopsis,
};
use chaos_vfs::Vfs;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use crate::embedding::{EmbeddingProvider, LocalEmbedder};
use crate::pg::PgRecallStore;
use crate::store::{
    RecallDoc, RecallError, RecallScope, RecallStore, RetrievalFilter, SearchRequest, SearchResult,
    normalize_embedding, validate_document,
};

pub struct RecallConfig {
    pub model_directory: PathBuf,
    pub embedding_concurrency: usize,
    pub search_timeout: Duration,
}

impl RecallConfig {
    pub fn new(model_directory: impl Into<PathBuf>) -> Self {
        Self {
            model_directory: model_directory.into(),
            embedding_concurrency: 2,
            search_timeout: Duration::from_secs(5),
        }
    }
}

/// Disabled backends perform no recall initialization, even with invalid config.
pub enum RecallState<E = LocalEmbedder> {
    Disabled,
    Ready(RecallService<PgRecallStore, E>),
}

impl RecallState {
    pub async fn from_mount(
        mount: Option<Vfs>,
        config: RecallConfig,
        cancellation: CancellationToken,
    ) -> Result<Self, RecallError> {
        initialize_with_loader(mount, config, cancellation, LocalEmbedder::load).await
    }
}

async fn initialize_with_loader<E, F, Fut>(
    mount: Option<Vfs>,
    config: RecallConfig,
    cancellation: CancellationToken,
    loader: F,
) -> Result<RecallState<E>, RecallError>
where
    E: EmbeddingProvider,
    F: FnOnce(PathBuf, usize, CancellationToken) -> Fut,
    Fut: Future<Output = Result<E, RecallError>>,
{
    // Must precede model loading, config validation, migration and task launch.
    let Some(Vfs::Postgres(pool)) = mount else {
        return Ok(RecallState::Disabled);
    };
    if config.embedding_concurrency == 0 || config.search_timeout.is_zero() {
        return Err(RecallError::InvalidInput("invalid recall configuration"));
    }
    let embedder = loader(
        config.model_directory.clone(),
        config.embedding_concurrency,
        cancellation.clone(),
    )
    .await?;
    let store = PgRecallStore::new(pool);
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(RecallError::Cancelled),
        result = async {
            store.migrate().await.map_err(RecallError::Backend)?;
            store.bind_model(embedder.fingerprint()).await
        } => result?,
    }
    Ok(RecallState::Ready(RecallService::new(
        store,
        embedder,
        config.search_timeout,
    )))
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub scope: RecallScope,
    pub include_global: bool,
    pub limit: usize,
    pub automatic_only: bool,
}

impl SearchOptions {
    pub fn new(scope: RecallScope) -> Self {
        Self {
            scope,
            include_global: false,
            limit: 10,
            automatic_only: false,
        }
    }

    fn validate(&self, query: &str) -> Result<(), RecallError> {
        self.scope.validate()?;
        if query.trim().is_empty() {
            return Err(RecallError::InvalidInput("query is empty"));
        }
        if self.limit == 0 || self.limit > 50 {
            return Err(RecallError::InvalidInput(
                "result limit must be between 1 and 50",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMethod {
    Semantic,
    Lexical,
}

impl RetrievalMethod {
    fn label(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Lexical => "lexical",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallMatch {
    pub scope: RecallScope,
    pub id: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub handle: crate::MemoryHandle,
    /// Reciprocal-rank fusion score, NOT cosine similarity.
    pub rank_score: f64,
    pub methods: Vec<RetrievalMethod>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallWarning {
    pub method: RetrievalMethod,
    pub failure_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResponse {
    pub matches: Vec<RecallMatch>,
    pub warnings: Vec<RecallWarning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<crate::UseReceipt>,
}

/// Injectable storage/embedding adapters keep retrieval policy independent of
/// tools and the kernel lifecycle. Production callers use `RecallState::from_mount`.
pub struct RecallService<S = PgRecallStore, E = LocalEmbedder> {
    store: Arc<S>,
    embedder: Arc<E>,
    timeout: Duration,
    generation: String,
}

impl<S: RecallStore, E: EmbeddingProvider> RecallService<S, E> {
    pub fn new(store: S, embedder: E, timeout: Duration) -> Self {
        Self {
            store: Arc::new(store),
            embedder: Arc::new(embedder),
            timeout,
            generation: uuid::Uuid::new_v4().to_string(),
        }
    }

    #[instrument(skip_all)]
    pub async fn store(
        &self,
        scope: RecallScope,
        id: String,
        content: String,
        metadata: serde_json::Value,
        cancellation: CancellationToken,
    ) -> Result<(), RecallError> {
        validate_document(&scope, &id, &content)?;
        let embedding = embed_text(self.embedder.as_ref(), &content, cancellation.clone()).await?;
        let doc = RecallDoc {
            scope,
            id,
            content,
            metadata,
            embedding,
            model_fingerprint: self.embedder.fingerprint().to_owned(),
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(RecallError::Cancelled),
            result = self.store.index(doc) => result,
        }
    }

    #[instrument(skip_all)]
    pub async fn delete(
        &self,
        scope: &RecallScope,
        id: &str,
        cancellation: CancellationToken,
    ) -> Result<(), RecallError> {
        scope.validate()?;
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(RecallError::Cancelled),
            result = self.store.delete(scope, id) => result,
        }
    }

    #[instrument(skip_all, fields(limit = options.limit))]
    pub async fn search(
        &self,
        query: &str,
        options: SearchOptions,
        cancellation: CancellationToken,
    ) -> Result<RecallResponse, RecallError> {
        options.validate(query)?;
        if cancellation.is_cancelled() {
            return Err(RecallError::Cancelled);
        }
        let state = Arc::new(Mutex::new(RequestState::default()));
        let executor = Arc::new(RecallExecutor {
            store: Arc::clone(&self.store),
            embedder: Arc::clone(&self.embedder),
            query: query.to_owned(),
            filter: RetrievalFilter {
                scope: options.scope,
                include_global: options.include_global,
                limit: options.limit * 5,
                model_fingerprint: self.embedder.fingerprint().to_owned(),
                automatic_only: options.automatic_only,
            },
            final_limit: options.limit,
            state: Arc::clone(&state),
        });
        let synopsis = Synopsis::new(Node::sequence([
            Node::action("validate", RecallAction::Validate),
            Node::parallel_all([
                Node::action(
                    "semantic",
                    RecallAction::Retrieve(RetrievalMethod::Semantic),
                ),
                Node::action("lexical", RecallAction::Retrieve(RetrievalMethod::Lexical)),
            ]),
            Node::action("fuse", RecallAction::Fuse),
        ]));
        let runner = Runner::new(synopsis, executor)
            .map_err(|error| RecallError::Workflow(error.to_string()))?;
        let started = Instant::now();
        // Dropping a timed-out runner aborts its JoinSet. Blocking inference
        // retains its semaphore permit until it actually finishes.
        let outcome = tokio::time::timeout(self.timeout, runner.run(cancellation)).await;
        let mut state = state.lock().await;
        let result = match outcome {
            Err(_) => Err(RecallError::Timeout),
            Ok(Err(error)) => Err(RecallError::Workflow(error.to_string())),
            Ok(Ok(Outcome::Cancelled)) => Err(RecallError::Cancelled),
            Ok(Ok(Outcome::Failure)) => Err(state.fatal.take().unwrap_or_else(|| {
                RecallError::Workflow("retrieval workflow terminated without a result".into())
            })),
            Ok(Ok(Outcome::Success)) => state.response.take().unwrap_or_else(|| {
                Err(RecallError::Workflow(
                    "retrieval workflow did not produce a result".into(),
                ))
            }),
        };
        match &result {
            Ok(response) => debug!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                count = response.matches.len(),
                degraded = !response.warnings.is_empty(),
                "recall search complete"
            ),
            Err(error) => debug!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                failure_kind = failure_kind(error),
                "recall search failed"
            ),
        }
        result
    }
}

/// Text ingestion and semantic queries use exactly the same embedding preflight.
async fn embed_text<E: EmbeddingProvider>(
    embedder: &E,
    text: &str,
    cancellation: CancellationToken,
) -> Result<Vec<f32>, RecallError> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(RecallError::Cancelled),
        result = embedder.embed(text, cancellation.clone()) => normalize_embedding(result?),
    }
}

#[derive(Clone, Copy)]
enum RecallAction {
    Validate,
    Retrieve(RetrievalMethod),
    Fuse,
}

type BranchResult = Result<Vec<SearchResult>, RecallError>;

#[derive(Default)]
struct RequestState {
    semantic: Option<BranchResult>,
    lexical: Option<BranchResult>,
    fatal: Option<RecallError>,
    response: Option<Result<RecallResponse, RecallError>>,
}

impl RequestState {
    fn branch(&mut self, method: RetrievalMethod) -> &mut Option<BranchResult> {
        match method {
            RetrievalMethod::Semantic => &mut self.semantic,
            RetrievalMethod::Lexical => &mut self.lexical,
        }
    }

    fn take_branch(&mut self, method: RetrievalMethod) -> Result<BranchResult, RecallError> {
        self.branch(method)
            .take()
            .ok_or_else(|| RecallError::Workflow(format!("missing {} branch", method.label())))
    }

    fn take_branches(&mut self) -> Result<(BranchResult, BranchResult), RecallError> {
        Ok((
            self.take_branch(RetrievalMethod::Semantic)?,
            self.take_branch(RetrievalMethod::Lexical)?,
        ))
    }
}

struct RecallExecutor<S, E> {
    store: Arc<S>,
    embedder: Arc<E>,
    query: String,
    filter: RetrievalFilter,
    final_limit: usize,
    state: Arc<Mutex<RequestState>>,
}

impl<S: RecallStore, E: EmbeddingProvider> ActionExecutor<RecallAction> for RecallExecutor<S, E> {
    fn execute(
        &self,
        _id: ActionId,
        action: RecallAction,
        cancellation: CancellationToken,
    ) -> ActionFuture {
        let store = Arc::clone(&self.store);
        let embedder = Arc::clone(&self.embedder);
        let state = Arc::clone(&self.state);
        let query = self.query.clone();
        let filter = self.filter.clone();
        let final_limit = self.final_limit;
        Box::pin(async move {
            match action {
                RecallAction::Validate => {
                    if let Err(error) = filter.validate() {
                        state.lock().await.fatal = Some(error);
                        return ActionOutcome::Failure;
                    }
                }
                RecallAction::Retrieve(method) => {
                    let started = Instant::now();
                    let result = match method {
                        RetrievalMethod::Semantic => {
                            async {
                                let query_vec =
                                    embed_text(embedder.as_ref(), &query, cancellation).await?;
                                store.search(&SearchRequest { filter, query_vec }).await
                            }
                            .await
                        }
                        RetrievalMethod::Lexical => store.lexical_search(&query, &filter).await,
                    };
                    debug!(
                        method = method.label(),
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        count = result.as_ref().map_or(0, Vec::len),
                        failure_kind = result.as_ref().err().map(failure_kind),
                        "recall retrieval branch complete"
                    );
                    let mut state = state.lock().await;
                    if matches!(
                        result,
                        Err(RecallError::Cancelled
                            | RecallError::Workflow(_)
                            | RecallError::ModelMismatch)
                    ) {
                        state.fatal = result.err();
                        return ActionOutcome::Failure;
                    }
                    *state.branch(method) = Some(result);
                    // Expected branch errors are data. ParallelAll must allow
                    // the healthy sibling to finish instead of cancelling it.
                }
                RecallAction::Fuse => {
                    let mut state = state.lock().await;
                    let (semantic, lexical) = match state.take_branches() {
                        Ok(branches) => branches,
                        Err(error) => {
                            state.fatal = Some(error);
                            return ActionOutcome::Failure;
                        }
                    };
                    state.response = Some(fuse(semantic, lexical, final_limit));
                }
            }
            ActionOutcome::Success
        })
    }
}

fn failure_kind(error: &RecallError) -> &'static str {
    match error {
        RecallError::Backend(_) => "backend",
        RecallError::ModelLoad(_) => "model_load",
        RecallError::DimMismatch { .. } | RecallError::InvalidEmbedding => "embedding",
        RecallError::ModelMismatch => "model_mismatch",
        RecallError::Cancelled => "cancelled",
        RecallError::Timeout => "timeout",
        RecallError::InvalidInput(_) => "invalid_input",
        RecallError::RetrievalFailed { .. } => "retrieval",
        RecallError::Workflow(_) => "workflow",
        RecallError::InvalidReceipt => "invalid_receipt",
    }
}

pub(crate) fn fuse(
    semantic: Result<Vec<SearchResult>, RecallError>,
    lexical: Result<Vec<SearchResult>, RecallError>,
    limit: usize,
) -> Result<RecallResponse, RecallError> {
    let (semantic, lexical) = match (semantic, lexical) {
        (Err(semantic), Err(lexical)) => {
            return Err(RecallError::RetrievalFailed {
                semantic: Box::new(semantic),
                lexical: Box::new(lexical),
            });
        }
        pair => pair,
    };
    let mut warnings = Vec::new();
    let mut matches: BTreeMap<(RecallScope, String), RecallMatch> = BTreeMap::new();
    for (method, results) in [
        (RetrievalMethod::Semantic, semantic),
        (RetrievalMethod::Lexical, lexical),
    ] {
        let results = match results {
            Ok(results) => results,
            Err(error) => {
                warnings.push(RecallWarning {
                    method,
                    failure_kind: failure_kind(&error).to_owned(),
                });
                continue;
            }
        };
        let mut seen = BTreeSet::new();
        for (index, result) in results.into_iter().enumerate() {
            let key = (result.scope.clone(), result.id.clone());
            if !seen.insert(key.clone()) {
                continue;
            }
            let entry = matches.entry(key).or_insert_with(|| RecallMatch {
                scope: result.scope,
                id: result.id,
                content: result.content,
                metadata: result.metadata,
                handle: result.handle,
                rank_score: 0.0,
                methods: Vec::new(),
            });
            entry.rank_score += 1.0 / (60.0 + (index + 1) as f64);
            entry.methods.push(method);
        }
    }
    let mut matches: Vec<_> = matches.into_values().collect();
    matches.sort_by(|left, right| {
        right
            .rank_score
            .total_cmp(&left.rank_score)
            .then_with(|| {
                right
                    .handle
                    .options
                    .charge
                    .total_cmp(&left.handle.options.charge)
            })
            .then_with(|| (&left.scope, &left.id).cmp(&(&right.scope, &right.id)))
    });
    matches.truncate(limit);
    Ok(RecallResponse {
        matches,
        warnings,
        receipt: None,
    })
}

mod memory;

#[cfg(test)]
mod tests;
