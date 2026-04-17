//! MCP tool: spool_submit — queue a batch of turns to a spool backend
//! and wire a cron row to poll the manifest until completion.

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::BackendCronStorage;
use crate::CronCtx;
use crate::CronScope;
use crate::CronServer;
use crate::CronStorage;
use crate::OwnerContext;
use crate::job::CreateJobParams;
use crate::spool_store::BackendSpoolStore;
use crate::spool_submit::submit_manifest_from_provider;
use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::TurnRequest;
use chaos_abi::shared_spool_registry;
use chaos_storage::ChaosStorageProvider;

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

    /// Cron schedule driving the poll loop (e.g. "5m", "15m", "1h").
    /// Defaults to 5 minutes.
    #[serde(default = "default_poll_schedule")]
    pub poll_schedule: String,

    /// Human-readable label for the poll cron row.
    #[serde(default)]
    pub name: Option<String>,

    /// Items to batch. Each becomes one TurnRequest in the backend batch.
    pub items: Vec<SpoolSubmitItem>,
}

fn default_poll_schedule() -> String {
    "5m".to_string()
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
        destructive = false,
        open_world = false
    )]
    async fn spool_submit(
        &self,
        ctx: CronCtx<'_>,
        params: Parameters<SpoolSubmitParams>,
    ) -> ToolResult {
        let owner = OwnerContext {
            project_path: ctx
                .environment
                .map(|environment| environment.cwd().to_string_lossy().to_string()),
            session_id: Some(ctx.session.id.clone()),
        };
        match execute(&params.0, None, &owner).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Standalone execution — callable from both MCP and kernel adapter.
pub async fn execute(
    params: &SpoolSubmitParams,
    provider: Option<&ChaosStorageProvider>,
    owner: &OwnerContext,
) -> Result<String, String> {
    if params.items.is_empty() {
        return Err("spool_submit requires at least one item".into());
    }

    let provider = match provider {
        Some(provider) => provider.clone(),
        None => ChaosStorageProvider::from_env(None).await?,
    };

    let registry = match shared_spool_registry() {
        Some(registry) => registry,
        None => {
            let msg = "no spool backends installed — set ANTHROPIC_API_KEY or XAI_API_KEY \
                       before starting chaos"
                .to_string();
            persist_failed_attempt(&provider, params, &msg).await;
            return Err(msg);
        }
    };
    if registry.get(&params.backend).is_none() {
        let available: Vec<&str> = registry.names().collect();
        let msg = format!(
            "backend '{}' not registered; available: {:?}",
            params.backend, available
        );
        persist_failed_attempt(&provider, params, &msg).await;
        return Err(msg);
    }

    // Validate the schedule BEFORE we push anything at the backend — a bad
    // schedule would leave us with a live batch and no way to poll it.
    if let Err(e) = crate::Schedule::parse(&params.poll_schedule) {
        let msg = format!("invalid poll_schedule: {e}");
        persist_failed_attempt(&provider, params, &msg).await;
        return Err(msg);
    }

    let project_path = match owner.project_path.clone() {
        Some(project_path) => project_path,
        None => {
            let msg = "current context is missing a project path for the poll cron row".to_string();
            persist_failed_attempt(&provider, params, &msg).await;
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
        &provider,
        &params.manifest_id,
        &params.backend,
        turn_items,
    )
    .await?;

    // Wire a cron row to drive the poll loop. Scope=Project because the
    // spool row lives in the shared DB and must be pollable across sessions.
    let storage = BackendCronStorage::from_provider(&provider)?;
    let name = params
        .name
        .clone()
        .unwrap_or_else(|| format!("spool-poll-{}", params.manifest_id));
    let cron_params = CreateJobParams::spool(
        name,
        params.poll_schedule.clone(),
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

    Ok(format!(
        "Spool submitted.\n  manifest_id: {}\n  backend: {}\n  batch_id: {}\n  items: {}\n  poll_cron_id: {}\n  poll_schedule: {}\n  next_poll_at: {}\n  replaced_poll_rows: {}",
        params.manifest_id,
        params.backend,
        batch_id,
        params.items.len(),
        job.id,
        job.schedule,
        job.next_run_at.map_or("none".into(), |t| t.to_string()),
        replaced_poll_rows,
    ))
}

async fn persist_failed_attempt(
    provider: &ChaosStorageProvider,
    params: &SpoolSubmitParams,
    error: &str,
) {
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
    let Ok(store) = BackendSpoolStore::from_provider(provider) else {
        return;
    };
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
mod tests {
    use super::*;
    use chaos_abi::SpoolBackend;
    use chaos_abi::SpoolError;
    use chaos_abi::SpoolItem;
    use chaos_abi::SpoolPhase;
    use chaos_abi::SpoolRegistry;
    use chaos_abi::SpoolStatusReport;
    use sqlx::Row;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Install the shared registry once per process. Subsequent test runs
    /// tolerate the second install being a no-op (OnceLock semantics).
    fn install_shared_registry_with_mock() {
        let mut registry = SpoolRegistry::new();
        registry.register(Arc::new(MockBackend::new()));
        // Ignore the result — tests running in the same process share the
        // single global. First install wins; follow-ups see the same mock.
        let _ = chaos_abi::set_shared_spool_registry(Arc::new(registry));
    }

    struct MockBackend {
        submitted: Mutex<Vec<Vec<String>>>,
    }
    impl MockBackend {
        fn new() -> Self {
            Self {
                submitted: Mutex::new(Vec::new()),
            }
        }
    }
    impl SpoolBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn submit(
            &self,
            items: Vec<(String, TurnRequest)>,
        ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
            let ids: Vec<String> = items.into_iter().map(|(id, _)| id).collect();
            self.submitted.lock().expect("poison").push(ids);
            Box::pin(async { Ok("mock-batch-99".into()) })
        }
        fn poll(
            &self,
            _: &str,
        ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>>
        {
            Box::pin(async {
                Ok(SpoolStatusReport {
                    phase: SpoolPhase::InProgress,
                    raw_provider_status: "mock".into(),
                })
            })
        }
        fn fetch_results(
            &self,
            _: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn cancel(
            &self,
            _: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn spool_submit_tool_batches_and_wires_poll_cron_row() {
        install_shared_registry_with_mock();

        let tmp = tempfile::tempdir().expect("tmp");
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let pool = provider.sqlite_pool_cloned().expect("sqlite pool");

        let params = SpoolSubmitParams {
            manifest_id: "manifest-tool-1".into(),
            backend: "mock".into(),
            poll_schedule: "5m".into(),
            name: Some("nightly-review".into()),
            items: vec![
                SpoolSubmitItem {
                    custom_id: "a".into(),
                    model: "mock-model".into(),
                    instructions: "be terse".into(),
                    user_message: "hi".into(),
                },
                SpoolSubmitItem {
                    custom_id: "b".into(),
                    model: "mock-model".into(),
                    instructions: "be terse".into(),
                    user_message: "there".into(),
                },
            ],
        };
        let owner = OwnerContext {
            project_path: Some("/tmp/project".into()),
            session_id: Some("session-1".into()),
        };

        let summary = execute(&params, Some(&provider), &owner)
            .await
            .expect("tool ok");
        assert!(summary.contains("mock-batch-99"), "summary={summary}");
        assert!(summary.contains("manifest-tool-1"), "summary={summary}");

        // Spool row landed in InProgress with both custom ids in payload_json.
        let spool_row = sqlx::query(
            "SELECT backend, batch_id, status, request_count, payload_json \
             FROM spool_jobs WHERE manifest_id = ?",
        )
        .bind("manifest-tool-1")
        .fetch_one(&pool)
        .await
        .expect("fetch spool row");
        let status: String = spool_row.get("status");
        let request_count: i64 = spool_row.get("request_count");
        let payload_json: String = spool_row.get("payload_json");
        assert_eq!(status, "InProgress");
        assert_eq!(request_count, 2);
        assert!(payload_json.contains("\"a\"") && payload_json.contains("\"b\""));

        // Cron row created with kind=spool and the matching manifest_id.
        let cron_row = sqlx::query(
            "SELECT name, schedule, kind, manifest_id, enabled, project_path \
             FROM cron_jobs WHERE manifest_id = ?",
        )
        .bind("manifest-tool-1")
        .fetch_one(&pool)
        .await
        .expect("fetch cron row");
        let name: String = cron_row.get("name");
        let schedule: String = cron_row.get("schedule");
        let kind: String = cron_row.get("kind");
        let enabled: i64 = cron_row.get("enabled");
        let project_path: String = cron_row.get("project_path");
        assert_eq!(name, "nightly-review");
        assert_eq!(schedule, "5m");
        assert_eq!(kind, "spool");
        assert_eq!(enabled, 1);
        assert_eq!(project_path, "/tmp/project");
    }

    #[tokio::test]
    async fn spool_submit_tool_rejects_unknown_backend() {
        install_shared_registry_with_mock();

        let tmp = tempfile::tempdir().expect("tmp");
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let pool = provider.sqlite_pool_cloned().expect("sqlite pool");

        let params = SpoolSubmitParams {
            manifest_id: "m2".into(),
            backend: "nonexistent".into(),
            poll_schedule: "5m".into(),
            name: None,
            items: vec![SpoolSubmitItem {
                custom_id: "a".into(),
                model: "m".into(),
                instructions: String::new(),
                user_message: "x".into(),
            }],
        };
        let owner = OwnerContext {
            project_path: Some("/tmp".into()),
            session_id: Some("s".into()),
        };

        let err = execute(&params, Some(&provider), &owner)
            .await
            .expect_err("should fail");
        assert!(err.contains("nonexistent"), "err={err}");

        let row = sqlx::query(
            "SELECT backend, batch_id, status, request_count, payload_json, error, submitted_at, completed_at \
             FROM spool_jobs WHERE manifest_id = ?",
        )
        .bind("m2")
        .fetch_one(&pool)
        .await
        .expect("fetch failed spool row");
        let backend_name: String = row.get("backend");
        let batch_id: Option<String> = row.get("batch_id");
        let status: String = row.get("status");
        let request_count: i64 = row.get("request_count");
        let payload_json: String = row.get("payload_json");
        let error: Option<String> = row.get("error");
        let submitted_at: Option<i64> = row.get("submitted_at");
        let completed_at: Option<i64> = row.get("completed_at");
        assert_eq!(backend_name, "nonexistent");
        assert!(batch_id.is_none());
        assert_eq!(status, "Failed");
        assert_eq!(request_count, 1);
        assert!(payload_json.contains("\"a\""));
        assert!(error.as_deref().unwrap_or_default().contains("nonexistent"));
        assert!(submitted_at.is_none());
        assert!(completed_at.is_some());
    }

    #[tokio::test]
    async fn spool_submit_tool_replaces_old_poll_cron_rows_for_same_manifest() {
        install_shared_registry_with_mock();

        let tmp = tempfile::tempdir().expect("tmp");
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let pool = provider.sqlite_pool_cloned().expect("sqlite pool");
        let owner = OwnerContext {
            project_path: Some("/tmp/project".into()),
            session_id: Some("session-1".into()),
        };

        let first = SpoolSubmitParams {
            manifest_id: "manifest-replace".into(),
            backend: "mock".into(),
            poll_schedule: "5m".into(),
            name: Some("first".into()),
            items: vec![SpoolSubmitItem {
                custom_id: "a".into(),
                model: "mock-model".into(),
                instructions: String::new(),
                user_message: "hi".into(),
            }],
        };
        execute(&first, Some(&provider), &owner)
            .await
            .expect("first submit");

        let second = SpoolSubmitParams {
            manifest_id: "manifest-replace".into(),
            backend: "mock".into(),
            poll_schedule: "15m".into(),
            name: Some("second".into()),
            items: vec![SpoolSubmitItem {
                custom_id: "b".into(),
                model: "mock-model".into(),
                instructions: String::new(),
                user_message: "there".into(),
            }],
        };
        let summary = execute(&second, Some(&provider), &owner)
            .await
            .expect("second submit");
        assert!(
            summary.contains("replaced_poll_rows: 1"),
            "summary={summary}"
        );

        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM cron_jobs WHERE kind = 'spool' AND manifest_id = ?",
        )
        .bind("manifest-replace")
        .fetch_one(&pool)
        .await
        .expect("count cron rows");
        let count: i64 = row.get("count");
        assert_eq!(count, 1);

        let cron_row = sqlx::query(
            "SELECT name, schedule FROM cron_jobs WHERE kind = 'spool' AND manifest_id = ?",
        )
        .bind("manifest-replace")
        .fetch_one(&pool)
        .await
        .expect("fetch cron row");
        let name: String = cron_row.get("name");
        let schedule: String = cron_row.get("schedule");
        assert_eq!(name, "second");
        assert_eq!(schedule, "15m");
    }
}
