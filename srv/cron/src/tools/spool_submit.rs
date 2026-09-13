//! MCP tool: spool_submit — queue a batch of turns to a spool backend
//! and wire a cron row to poll the manifest until completion.

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::BackendCronStorage;
use crate::CronCtx;
use crate::CronScope;
use crate::CronServer;
use crate::CronStorage;
use crate::OwnerContext;
use crate::job::CreateJobParams;
use crate::spool_store::BackendSpoolStore;
use crate::spool_submit::submit_manifest_from_provider;
use crate::tools::cron_vfs;
use crate::tools::owner_context_from_cron_ctx;
use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::TurnRequest;
use chaos_abi::shared_spool_registry;
use chaos_vfs::ChaosVfs;

/// Parameters for the spool_submit tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SpoolSubmitParams {
    /// Caller-assigned manifest id. Idempotent: resubmitting the same id
    /// replaces the prior row (and the poll cron row if present).
    pub manifest_id: String,

    /// Registered backend name ("anthropic", "xai"). Must be configured
    /// via env (ANTHROPIC_API_KEY / XAI_API_KEY) at kernel boot.
    pub backend: String,

    /// Schedule driving the poll loop, e.g. `{"kind":"interval","seconds":300}`.
    /// Defaults to every 5 minutes.
    #[serde(default = "default_poll_schedule")]
    pub poll_schedule: crate::schedule::Schedule,

    /// Human-readable label for the poll cron row.
    #[serde(default)]
    pub name: Option<String>,

    /// Items to batch. Each becomes one TurnRequest in the backend batch.
    pub items: Vec<SpoolSubmitItem>,
}

fn default_poll_schedule() -> crate::schedule::Schedule {
    crate::schedule::Schedule::Interval { seconds: 300 }
}

/// One batch item. Minimal shape: system prompt + single user message.
/// Richer request shapes can be added later without breaking this schema.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SpoolSubmitItem {
    /// Opaque caller id. Appears back in the result keyed by this string.
    pub custom_id: String,
    /// Provider-specific model slug.
    pub model: String,
    /// System prompt.
    #[serde(default)]
    pub instructions: String,
    /// Single user message that forms the conversation input.
    pub user_message: String,
}

impl CronServer {
    #[mcp_tool(
        name = "spool_submit",
        description = "Queue a batch of turns to a spool backend and schedule a cron job that polls until the batch completes.",
        destructive = false
    )]
    async fn spool_submit(
        &self,
        ctx: CronCtx<'_>,
        params: Parameters<SpoolSubmitParams>,
    ) -> ToolResult {
        let owner = owner_context_from_cron_ctx(ctx);
        match execute_structured(&params.0, &owner).await {
            Ok(value) => ToolOutput::structured(value)
                .map_err(|e| ToolError::Execution(format!("non-object tool output: {e}"))),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Standalone execution — callable from both MCP and kernel adapter.
pub async fn execute(params: &SpoolSubmitParams, owner: &OwnerContext) -> Result<String, String> {
    execute_structured(params, owner)
        .await
        .map(|value| value.to_string())
}

pub async fn execute_structured(
    params: &SpoolSubmitParams,
    owner: &OwnerContext,
) -> Result<serde_json::Value, String> {
    execute_structured_on(cron_vfs()?, params, owner).await
}

async fn execute_structured_on(
    provider: &ChaosVfs,
    params: &SpoolSubmitParams,
    owner: &OwnerContext,
) -> Result<serde_json::Value, String> {
    if params.items.is_empty() {
        return Err("spool_submit requires at least one item".into());
    }

    let registry = match shared_spool_registry() {
        Some(registry) => registry,
        None => {
            let msg = "no spool backends installed — set ANTHROPIC_API_KEY or XAI_API_KEY \
                       before starting chaos"
                .to_string();
            persist_failed_attempt(provider, params, &msg).await;
            return Err(msg);
        }
    };
    if registry.get(&params.backend).is_none() {
        let available: Vec<&str> = registry.names().collect();
        let msg = format!(
            "backend '{}' not registered; available: {:?}",
            params.backend, available
        );
        persist_failed_attempt(provider, params, &msg).await;
        return Err(msg);
    }

    // Validate the schedule BEFORE we push anything at the backend — a bad
    // schedule would leave us with a live batch and no way to poll it.
    if let Err(e) = params.poll_schedule.validate() {
        let msg = format!("invalid poll_schedule: {e}");
        persist_failed_attempt(provider, params, &msg).await;
        return Err(msg);
    }
    let poll_schedule_json = params.poll_schedule.to_json();

    let project_path = match owner.project_path.clone() {
        Some(project_path) => project_path,
        None => {
            let msg = "current context is missing a project path for the poll cron row".to_string();
            persist_failed_attempt(provider, params, &msg).await;
            return Err(msg);
        }
    };

    let turn_items: Vec<(String, TurnRequest)> = params
        .items
        .iter()
        .map(|item| (item.custom_id.clone(), item_to_turn_request(item)))
        .collect();

    let batch_id = submit_manifest_from_provider(
        &registry,
        provider,
        &params.manifest_id,
        &params.backend,
        turn_items,
    )
    .await?;

    // Wire a cron row to drive the poll loop. Scope=Project because the
    // spool row lives in the shared DB and must be pollable across sessions.
    let storage = BackendCronStorage::from_provider(provider);
    let name = params
        .name
        .clone()
        .unwrap_or_else(|| format!("spool-poll-{}", params.manifest_id));
    let cron_params = CreateJobParams::spool(
        name,
        poll_schedule_json,
        params.manifest_id.clone(),
        CronScope::Project,
        Some(project_path),
        None,
    );
    let job = storage
        .create(&cron_params)
        .await
        .map_err(|e| format!("persist poll cron row: {e}"))?;
    let replaced_poll_rows = storage
        .delete_spool_jobs_for_manifest_except(&params.manifest_id, Some(&job.id))
        .await
        .map_err(|e| format!("cleanup replaced poll cron rows: {e}"))?;

    Ok(json!({
        "status": "submitted",
        "manifest_id": params.manifest_id,
        "backend": params.backend,
        "batch_id": batch_id,
        "item_count": params.items.len(),
        "poll_cron_id": job.id,
        "poll_schedule": job.schedule,
        "next_poll_at": job.next_run_at.map(|t| t.to_string()),
        "replaced_poll_rows": replaced_poll_rows,
    }))
}

async fn persist_failed_attempt(provider: &ChaosVfs, params: &SpoolSubmitParams, error: &str) {
    if params.items.is_empty() {
        return;
    }

    let Ok(request_count) = u32::try_from(params.items.len()) else {
        return;
    };
    let custom_ids: Vec<&str> = params
        .items
        .iter()
        .map(|item| item.custom_id.as_str())
        .collect();
    let Ok(payload_json) = serde_json::to_string(&custom_ids) else {
        return;
    };
    let store = BackendSpoolStore::from_provider(provider);
    let _ = store
        .insert_queued(
            &params.manifest_id,
            &params.backend,
            request_count,
            &payload_json,
        )
        .await;
    let _ = store.mark_submit_failed(&params.manifest_id, error).await;
}

fn item_to_turn_request(item: &SpoolSubmitItem) -> TurnRequest {
    TurnRequest {
        model: item.model.clone(),
        instructions: item.instructions.clone(),
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: item.user_message.clone(),
            }],
            end_turn: None,
            phase: None,
        }],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    }
}

/// Returns the auto-generated `ToolInfo` for schema extraction by core.
pub fn tool_info() -> ToolInfo {
    CronServer::spool_submit_tool_info()
}

pub fn mount(
    router: mcp_host::registry::router::McpToolRouter<CronServer>,
) -> mcp_host::registry::router::McpToolRouter<CronServer> {
    router.with_tool(
        CronServer::spool_submit_tool_info(),
        CronServer::spool_submit_handler,
        None,
    )
}

#[cfg(test)]
#[path = "spool_submit/tests.rs"]
mod tests;
