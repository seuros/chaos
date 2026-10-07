use std::path::Path;

use chaos_recall::{
    EmbeddingProvider, MemoryOptions, MemoryStore, RecallResponse, RecallScope, RecallService,
    SearchOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::function_tool::FunctionCallError;
use crate::runtime_db::RuntimeDbHandle;
use crate::tools::context::{FunctionToolOutput, ToolInvocation};
use crate::tools::registry::{ToolHandler, ToolKind};

mod schema;
mod source;
pub(crate) use schema::tool;

pub(crate) const SEARCH: &str = "recall_search";
pub(crate) const NAMES: [&str; 5] = [
    SEARCH,
    "recall_store",
    "recall_delete",
    "recall_open",
    "recall_use",
];
const QUERY_BYTES: usize = 4096;
const ID_BYTES: usize = 256;
const CONTENT_BYTES: usize = 16_384;
const METADATA_BYTES: usize = 8192;
const SNIPPET_BYTES: usize = 4096;
const OUTPUT_BYTES: usize = 65_536;

pub(crate) struct Handler;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    #[default]
    Project,
    Session,
    Global,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WriteScope {
    #[default]
    Project,
    Session,
}

impl From<WriteScope> for Scope {
    fn from(scope: WriteScope) -> Self {
        match scope {
            WriteScope::Project => Self::Project,
            WriteScope::Session => Self::Session,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    scope: Scope,
    #[serde(default)]
    include_global: bool,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreArgs {
    id: String,
    content: String,
    #[serde(default)]
    scope: WriteScope,
    #[serde(default = "empty_metadata")]
    metadata: Value,
    #[serde(default)]
    memory: MemoryOptions,
}

fn empty_metadata() -> Value {
    json!({})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    id: String,
    #[serde(default)]
    scope: WriteScope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecalledArgs {
    receipt: String,
    id: String,
    #[serde(default)]
    scope: Scope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    receipt: String,
    id: String,
    #[serde(default)]
    scope: Scope,
    #[serde(default)]
    source_index: usize,
}

enum Request {
    Search(SearchArgs),
    Store(StoreArgs),
    Delete(DeleteArgs),
    Open(OpenArgs),
    Use(RecalledArgs),
}

impl Request {
    fn parse(name: &str, arguments: &str, mutation: bool) -> Result<Self, FunctionCallError> {
        if !matches!(name, SEARCH | "recall_open") && !mutation {
            return Err(error(
                "Recall writes are unavailable in this read-only mode.",
            ));
        }
        // Do not sanitize JSON string leaves: provenance metadata must be verbatim.
        fn parse<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, FunctionCallError> {
            serde_json::from_str(arguments)
                .map_err(|err| error(format!("Invalid recall arguments: {err}")))
        }
        let request = match name {
            SEARCH => Self::Search(parse(arguments)?),
            "recall_store" => Self::Store(parse(arguments)?),
            "recall_delete" => Self::Delete(parse(arguments)?),
            "recall_open" => Self::Open(parse(arguments)?),
            "recall_use" => Self::Use(parse(arguments)?),
            _ => return Err(error("Unsupported recall tool.")),
        };
        match &request {
            Self::Search(args) => {
                bounded_text("query", &args.query, QUERY_BYTES)?;
                if !(1..=50).contains(&args.limit) {
                    return Err(error("Recall limit must be between 1 and 50."));
                }
            }
            Self::Store(args) => {
                bounded_text("id", &args.id, ID_BYTES)?;
                bounded_text("content", &args.content, CONTENT_BYTES)?;
                if serialize(&args.metadata)?.len() > METADATA_BYTES {
                    return Err(error("Recall metadata exceeds 8192 serialized bytes."));
                }
                args.memory.validate().map_err(recall_error)?;
                if serialize(&args.memory)?.len() > METADATA_BYTES {
                    return Err(error("Memory options exceed 8192 serialized bytes."));
                }
            }
            Self::Delete(args) => bounded_text("id", &args.id, ID_BYTES)?,
            Self::Open(args) => validate_recalled(&args.receipt, &args.id)?,
            Self::Use(args) => {
                validate_recalled(&args.receipt, &args.id)?;
                if matches!(args.scope, Scope::Global) {
                    return Err(error("Global recall is read-only for native tools."));
                }
            }
        }
        Ok(request)
    }

    fn scope(&self) -> Scope {
        match self {
            Self::Search(args) => args.scope,
            Self::Store(args) => args.scope.into(),
            Self::Delete(args) => args.scope.into(),
            Self::Open(args) => args.scope,
            Self::Use(args) => args.scope,
        }
    }
}

fn validate_recalled(receipt: &str, id: &str) -> Result<(), FunctionCallError> {
    bounded_text("id", id, ID_BYTES)?;
    uuid::Uuid::parse_str(receipt).map_err(|_| error("Invalid recall receipt."))?;
    Ok(())
}

impl ToolHandler for Handler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn is_mutating(&self, invocation: &ToolInvocation) -> impl Future<Output = bool> + Send + '_ {
        let mutating = !matches!(invocation.tool_name.as_str(), SEARCH | "recall_open");
        async move { mutating }
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments =
            super::extract_function_arguments(invocation.payload, &invocation.tool_name)?;
        let request = Request::parse(
            &invocation.tool_name,
            &arguments,
            invocation.turn.mode_capabilities.mutation,
        )?;
        let service = invocation
            .session
            .services
            .recall
            .as_ref()
            .and_then(|runtime| runtime.ready_service())
            .ok_or_else(|| {
                error(
                    "Recall is unavailable. Enable the recall group on PostgreSQL; \
                failed activation can be retried with enable_tools.",
                )
            })?;
        let scope = match request.scope() {
            Scope::Session => RecallScope::Session(invocation.session.conversation_id.to_string()),
            Scope::Global => RecallScope::Global,
            Scope::Project => {
                let installation =
                    crate::user_settings::installation_id(&invocation.turn.config.chaos_home)
                        .map_err(|_| error("Cannot resolve the local installation identity."))?;
                project_scope(
                    invocation.session.services.runtime_db.as_ref(),
                    &installation,
                    &invocation.turn.cwd,
                )
                .await?
            }
        };
        // The tool dispatcher aborts/drops this future on cancellation. Dropping
        // search drops the library Runner, aborting its async retrieval branches.
        let owner = invocation.session.conversation_id.to_string();
        let output = match request {
            Request::Open(args) => {
                let memory = service
                    .recalled_memory(&owner, &args.receipt, &scope, &args.id)
                    .await
                    .map_err(recall_error)?;
                let source = memory
                    .options
                    .sources
                    .get(args.source_index)
                    .ok_or_else(|| error("No source at that index."))?
                    .clone();
                let policy = invocation
                    .session
                    .permission_snapshot(&invocation.turn)
                    .await
                    .effective_vfs_policy();
                let cwd = invocation.turn.cwd.clone();
                tokio::task::spawn_blocking(move || source::read(&source, &policy, &cwd))
                    .await
                    .map_err(|_| error("Source reader failed."))?
                    .map_err(recall_error)?
            }
            request => {
                execute_for_owner(
                    service.as_ref(),
                    request,
                    scope,
                    &owner,
                    CancellationToken::new(),
                )
                .await?
            }
        };
        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}

pub(crate) async fn project_scope(
    db: Option<&RuntimeDbHandle>,
    installation: &str,
    cwd: &Path,
) -> Result<RecallScope, FunctionCallError> {
    let missing = || {
        error(
            "No registered project checkout for recall. Register this project or use scope=\"session\".",
        )
    };
    let db = db.ok_or_else(missing)?;
    let checkout = db
        .planning_checkout(installation, cwd)
        .await
        .map_err(|_| error("Cannot resolve the registered project checkout for recall."))?
        .ok_or_else(missing)?;
    Ok(RecallScope::Project(checkout.project_id))
}

#[cfg(test)]
async fn execute<S: MemoryStore, E: EmbeddingProvider>(
    service: &RecallService<S, E>,
    request: Request,
    scope: RecallScope,
    cancellation: CancellationToken,
) -> Result<String, FunctionCallError> {
    execute_for_owner(service, request, scope, "test-owner", cancellation).await
}

async fn execute_for_owner<S: MemoryStore, E: EmbeddingProvider>(
    service: &RecallService<S, E>,
    request: Request,
    scope: RecallScope,
    owner: &str,
    cancellation: CancellationToken,
) -> Result<String, FunctionCallError> {
    if !matches!(request, Request::Search(_) | Request::Open(_)) && scope == RecallScope::Global {
        return Err(error("Global recall is read-only for native tools."));
    }
    match request {
        Request::Search(args) => {
            let response = service
                .search(
                    &args.query,
                    SearchOptions {
                        scope: scope.clone(),
                        include_global: args.include_global,
                        limit: args.limit,
                        automatic_only: false,
                    },
                    cancellation,
                )
                .await
                .map_err(recall_error)?;
            let mut output = bounded_output(scope, response)?;
            let mut selected = RecallResponse {
                matches: output
                    .matches
                    .iter()
                    .map(|item| item.result.clone())
                    .collect(),
                warnings: Vec::new(),
                receipt: None,
            };
            service
                .attach_receipt(owner, &mut selected)
                .await
                .map_err(recall_error)?;
            output.receipt = selected.receipt;
            serialize(&output)
        }
        Request::Store(args) => {
            service
                .store_memory(
                    scope.clone(),
                    args.id.clone(),
                    args.content,
                    args.metadata,
                    args.memory,
                    cancellation,
                )
                .await
                .map_err(recall_error)?;
            serialize(&json!({"status": "stored", "scope": scope, "id": args.id}))
        }
        Request::Delete(args) => {
            service
                .delete(&scope, &args.id, cancellation)
                .await
                .map_err(recall_error)?;
            serialize(&json!({"status": "deleted", "scope": scope, "id": args.id}))
        }
        Request::Use(args) => serialize(
            &service
                .use_memory(owner, &args.receipt, &scope, &args.id)
                .await
                .map_err(recall_error)?,
        ),
        Request::Open(_) => Err(error(
            "Source opening requires the active filesystem policy.",
        )),
    }
}

#[derive(Serialize)]
struct SearchOutput {
    scope: RecallScope,
    matches: Vec<MatchOutput>,
    warnings: Vec<chaos_recall::RecallWarning>,
    truncated: bool,
    omitted_matches: usize,
    receipt: Option<chaos_recall::UseReceipt>,
}

#[derive(Serialize)]
struct MatchOutput {
    #[serde(flatten)]
    result: chaos_recall::RecallMatch,
    content_truncated: bool,
    metadata_omitted: bool,
}

#[cfg(test)]
fn search_output(
    scope: RecallScope,
    response: RecallResponse,
) -> Result<String, FunctionCallError> {
    serialize(&bounded_output(scope, response)?)
}

fn bounded_output(
    scope: RecallScope,
    response: RecallResponse,
) -> Result<SearchOutput, FunctionCallError> {
    let mut output = SearchOutput {
        scope,
        matches: Vec::new(),
        warnings: response.warnings,
        truncated: false,
        omitted_matches: 0,
        receipt: response.receipt,
    };
    for mut result in response.matches {
        let content_truncated = truncate_utf8(&mut result.content, SNIPPET_BYTES);
        let metadata_omitted = serialize(&result.metadata)?.len() > METADATA_BYTES;
        if metadata_omitted {
            result.metadata = Value::Null;
        }
        output.truncated |= content_truncated || metadata_omitted;
        output.matches.push(MatchOutput {
            result,
            content_truncated,
            metadata_omitted,
        });
    }
    // The library emits at most two short branch warnings. Bound them defensively
    // as well, so arbitrary adapter diagnostics cannot bypass the output budget.
    if output.warnings.len() > 2 {
        output.warnings.truncate(2);
        output.truncated = true;
    }
    for warning in &mut output.warnings {
        output.truncated |= truncate_utf8(&mut warning.failure_kind, 256);
    }
    loop {
        let text = serialize(&output)?;
        if text.len() <= OUTPUT_BYTES - 200 {
            return Ok(output);
        }
        if output.matches.pop().is_none() {
            return Err(error(
                "Recall response provenance exceeds the output byte budget.",
            ));
        }
        output.truncated = true;
        output.omitted_matches += 1;
    }
}

pub(crate) fn truncate_utf8(text: &mut String, maximum: usize) -> bool {
    if text.len() <= maximum {
        return false;
    }
    let mut boundary = maximum;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    true
}

fn bounded_text(name: &str, text: &str, maximum: usize) -> Result<(), FunctionCallError> {
    if text.trim().is_empty() || text.len() > maximum {
        return Err(error(format!(
            "Recall {name} must be nonempty and at most {maximum} UTF-8 bytes."
        )));
    }
    Ok(())
}

fn serialize(value: &impl Serialize) -> Result<String, FunctionCallError> {
    serde_json::to_string(value).map_err(|_| error("Cannot serialize the recall response."))
}

fn recall_error(err: chaos_recall::RecallError) -> FunctionCallError {
    // Backend errors may contain SQL, query text or document contents. Do not
    // reflect those diagnostics or log user memory through the native boundary.
    use chaos_recall::RecallError;
    match err {
        RecallError::Backend(_) | RecallError::RetrievalFailed { .. } => {
            error("Recall storage/retrieval failed. Check PostgreSQL availability and permissions.")
        }
        RecallError::ModelLoad(_) => {
            error("Recall model is unavailable; check local artifacts and restart.")
        }
        other => error(other.to_string()),
    }
}

fn error(message: impl Into<String>) -> FunctionCallError {
    FunctionCallError::RespondToModel(message.into())
}

#[cfg(test)]
mod tests;
