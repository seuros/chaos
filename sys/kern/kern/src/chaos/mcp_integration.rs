use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::SandboxState;
use crate::config::types::McpServerConfig;
use crate::mcp::auth::compute_auth_statuses;
use crate::mcp::oauth_types::OAuthCredentialsStoreMode;
use crate::protocol::McpServerRefreshConfig;
use breaker_machines::CircuitBreaker;
use chaos_ipc::api::McpServerElicitationRequest;
use chaos_ipc::api::McpServerElicitationRequestParams;
use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::mcp::CallToolResult;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::McpServersRefreshedEvent;
use chaos_ipc::protocol::McpStartupFailure;
use chaos_mcp_runtime::ElicitationResponse;
use chaos_mcp_runtime::ListResourceTemplatesResult;
use chaos_mcp_runtime::ListResourcesResult;
use chaos_mcp_runtime::ListTasksResult;
use chaos_mcp_runtime::McpRequestId as RequestId;
use chaos_mcp_runtime::McpTask;
use chaos_mcp_runtime::McpToolCallResult;
use chaos_mcp_runtime::PaginatedRequestParams;
use chaos_mcp_runtime::ReadResourceRequestParams;
use chaos_mcp_runtime::ReadResourceResult;
use chaos_mcp_runtime::manager::McpConnectionManager;
use tokio::sync::oneshot;
use tracing::warn;

use super::Session;
use super::TurnContext;

const HALF_OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// Breaker state bundled with open-timestamp for manual half-open transitions.
///
/// `breaker-machines` `call()` is sync-only so we can't use it for async
/// operations. The manual `record_*` API doesn't drive Open→HalfOpen
/// transitions, so we track `opened_at` ourselves and `reset()` after the
/// configured timeout. This is coarser than true HalfOpen (it goes straight
/// to Closed, allowing all traffic through) but prevents permanent latching.
///
/// TODO: add `try_half_open(&mut self)` to `breaker-machines` for proper
/// Open→HalfOpen transitions without requiring `call()`.
struct BreakerState {
    breaker: CircuitBreaker,
    opened_at: Option<Instant>,
}

/// Process-wide registry of per-server circuit breakers.
///
/// Each entry is wrapped in `Arc<Mutex<>>` so callers can hold a reference
/// while the registry lock is released during the actual call. Initialised
/// lazily on first access; this is intentionally a singleton so all session
/// instances share the same fault-detection state for a given MCP server.
static MCP_CIRCUIT_BREAKERS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, Arc<std::sync::Mutex<BreakerState>>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Retrieve (or create) the circuit breaker state for `server_name`.
fn mcp_circuit_breaker(server_name: &str) -> Arc<std::sync::Mutex<BreakerState>> {
    let mut map = MCP_CIRCUIT_BREAKERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(server_name.to_string())
        .or_insert_with(|| {
            Arc::new(std::sync::Mutex::new(BreakerState {
                breaker: CircuitBreaker::builder(server_name)
                    .failure_threshold(5)
                    .failure_window_secs(60.0)
                    .half_open_timeout_secs(30.0)
                    .success_threshold(2)
                    .build(),
                opened_at: None,
            }))
        })
        .clone()
}

/// Wraps an async MCP operation with the per-server circuit breaker.
///
/// Fails fast when the breaker is open and the half-open timeout hasn't
/// elapsed. Records success/failure to drive state transitions.
async fn with_circuit_breaker<T, F, Fut>(server: &str, op: F) -> anyhow::Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let breaker = mcp_circuit_breaker(server);

    {
        let mut guard = breaker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.breaker.is_open() {
            if let Some(opened_at) = guard.opened_at {
                if opened_at.elapsed() >= HALF_OPEN_TIMEOUT {
                    // Timeout elapsed — reset to allow a probe call through.
                    guard.breaker.reset();
                    guard.opened_at = None;
                    warn!("MCP server '{server}' circuit reset after timeout — probing");
                } else {
                    anyhow::bail!(
                        "MCP server '{server}' circuit open — too many recent failures, backing off"
                    );
                }
            } else {
                anyhow::bail!(
                    "MCP server '{server}' circuit open — too many recent failures, backing off"
                );
            }
        }
    }

    let start = Instant::now();
    let result = op().await;
    let duration = start.elapsed().as_secs_f64();

    {
        let mut guard = breaker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &result {
            Ok(_) => {
                guard.breaker.record_success_and_maybe_close(duration);
                if !guard.breaker.is_open() {
                    guard.opened_at = None;
                }
            }
            Err(_) => {
                let was_open = guard.breaker.is_open();
                guard.breaker.record_failure_and_maybe_trip(duration);
                if !was_open && guard.breaker.is_open() {
                    guard.opened_at = Some(Instant::now());
                }
            }
        }
    }

    result
}

/// Generates a `Session` method that delegates an async MCP call through the
/// per-server circuit breaker.
///
/// Syntax:
/// ```
/// mcp_delegate!(vis fn name(server [, arg: Ty]*) -> Ret => manager_method);
/// ```
/// where `manager_method` is the method name on `McpConnectionManager` (may
/// differ from the outer name when the public name has a prefix).
macro_rules! mcp_delegate {
    (
        $vis:vis fn $name:ident (
            server $(, $arg:ident : $arg_ty:ty)*
        ) -> $ret:ty => $mgr_method:ident
    ) => {
        $vis async fn $name(
            &self,
            server: &str,
            $($arg: $arg_ty),*
        ) -> anyhow::Result<$ret> {
            let registry = self.services.mcp_registry.clone();
            let breaker_server = server.to_string();
            let dispatch_server = breaker_server.clone();
            with_circuit_breaker(&breaker_server, move || async move {
                registry
                    .execute(&dispatch_server, move |manager, server| async move {
                        manager.$mgr_method(&server, $($arg),*).await
                    })
                    .await
            })
            .await
        }
    };
}

impl Session {
    mcp_delegate!(pub fn list_resources(server, params: Option<PaginatedRequestParams>) -> ListResourcesResult => list_resources);
    mcp_delegate!(pub fn list_resource_templates(server, params: Option<PaginatedRequestParams>) -> ListResourceTemplatesResult => list_resource_templates);
    mcp_delegate!(pub fn read_resource(server, params: ReadResourceRequestParams) -> ReadResourceResult => read_resource);
    mcp_delegate!(pub fn list_mcp_tasks(server) -> ListTasksResult => list_tasks);

    pub async fn call_tool_async(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
        ttl: Option<u64>,
    ) -> anyhow::Result<McpTask> {
        let registry = self.services.mcp_registry.clone();
        let breaker_server = server.to_string();
        let dispatch_server = breaker_server.clone();
        let tool = tool.to_string();
        with_circuit_breaker(&breaker_server, move || async move {
            registry
                .execute(&dispatch_server, move |manager, server| async move {
                    manager
                        .call_tool_async(&server, &tool, arguments, meta, ttl)
                        .await
                })
                .await
        })
        .await
    }

    pub(crate) async fn get_mcp_task(
        &self,
        server: &str,
        task_id: &str,
    ) -> anyhow::Result<McpTask> {
        let registry = self.services.mcp_registry.clone();
        let breaker_server = server.to_string();
        let dispatch_server = breaker_server.clone();
        let task_id = task_id.to_string();
        with_circuit_breaker(&breaker_server, move || async move {
            registry
                .execute(&dispatch_server, move |manager, server| async move {
                    manager.get_task(&server, &task_id).await
                })
                .await
        })
        .await
    }

    pub async fn get_mcp_task_result(
        &self,
        server: &str,
        task_id: &str,
    ) -> anyhow::Result<McpToolCallResult> {
        let registry = self.services.mcp_registry.clone();
        let breaker_server = server.to_string();
        let dispatch_server = breaker_server.clone();
        let task_id = task_id.to_string();
        with_circuit_breaker(&breaker_server, move || async move {
            registry
                .execute(&dispatch_server, move |manager, server| async move {
                    manager.get_task_result(&server, &task_id).await
                })
                .await
        })
        .await
    }

    pub async fn cancel_mcp_task(&self, server: &str, task_id: &str) -> anyhow::Result<McpTask> {
        let registry = self.services.mcp_registry.clone();
        let breaker_server = server.to_string();
        let dispatch_server = breaker_server.clone();
        let task_id = task_id.to_string();
        with_circuit_breaker(&breaker_server, move || async move {
            registry
                .execute(&dispatch_server, move |manager, server| async move {
                    manager.cancel_task(&server, &task_id).await
                })
                .await
        })
        .await
    }

    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
    ) -> anyhow::Result<CallToolResult> {
        let registry = self.services.mcp_registry.clone();
        let breaker_server = server.to_string();
        let dispatch_server = breaker_server.clone();
        let tool = tool.to_string();
        with_circuit_breaker(&breaker_server, move || async move {
            registry
                .execute(&dispatch_server, move |manager, server| async move {
                    manager.call_tool(&server, &tool, arguments, meta).await
                })
                .await
        })
        .await
    }

    /// Host-only MCP path for attaching attested reviewer provenance.
    #[expect(
        dead_code,
        reason = "v0.9 review orchestration will call this host-only foundation"
    )]
    pub(crate) async fn call_tool_with_review_provenance(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
        provenance: chaos_mcp_runtime::TrustedReviewProvenance,
    ) -> anyhow::Result<CallToolResult> {
        let registry = self.services.mcp_registry.clone();
        let breaker_server = server.to_string();
        let dispatch_server = breaker_server.clone();
        let tool = tool.to_string();
        with_circuit_breaker(&breaker_server, move || async move {
            registry
                .execute(&dispatch_server, move |manager, server| async move {
                    manager
                        .call_tool_with_review_provenance(
                            &server, &tool, arguments, meta, provenance,
                        )
                        .await
                })
                .await
        })
        .await
    }

    pub(crate) async fn parse_mcp_tool_name(
        &self,
        name: &str,
        namespace: &Option<String>,
    ) -> Option<(String, String)> {
        let tool_name = if let Some(namespace) = namespace {
            if name.starts_with(namespace.as_str()) {
                name
            } else {
                &format!("{namespace}{name}")
            }
        } else {
            name
        };
        self.services
            .mcp_registry
            .current_manager()
            .parse_tool_name(tool_name)
            .await
    }

    pub async fn request_mcp_server_elicitation(
        &self,
        turn_context: &TurnContext,
        request_id: RequestId,
        params: McpServerElicitationRequestParams,
    ) -> Option<ElicitationResponse> {
        let server_name = params.server_name.clone();
        let request = match params.request {
            McpServerElicitationRequest::Form {
                meta,
                message,
                requested_schema,
            } => {
                let requested_schema = match serde_json::to_value(requested_schema) {
                    Ok(requested_schema) => requested_schema,
                    Err(err) => {
                        warn!(
                            "failed to serialize MCP elicitation schema for \
                             server_name: {server_name}, \
                             request_id: {request_id}: {err:#}"
                        );
                        return None;
                    }
                };
                chaos_ipc::approvals::ElicitationRequest::Form {
                    meta,
                    message,
                    requested_schema,
                }
            }
            McpServerElicitationRequest::Url {
                meta,
                message,
                url,
                elicitation_id,
            } => chaos_ipc::approvals::ElicitationRequest::Url {
                meta,
                message,
                url,
                elicitation_id,
            },
        };

        let (tx_response, rx_response) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_elicitation(
                        server_name.clone(),
                        request_id.clone(),
                        tx_response,
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!(
                "Overwriting existing pending elicitation for \
                 server_name: {server_name}, request_id: {request_id}"
            );
        }
        let id = match &request_id {
            RequestId::String(value) => chaos_ipc::mcp::RequestId::String(value.clone()),
            RequestId::Number(value) => match value.as_i64() {
                Some(value) => chaos_ipc::mcp::RequestId::Integer(value),
                None => chaos_ipc::mcp::RequestId::String(value.to_string()),
            },
        };
        let event = EventMsg::ElicitationRequest(ElicitationRequestEvent {
            turn_id: params.turn_id,
            server_name,
            id,
            request,
        });
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    pub async fn resolve_elicitation(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> anyhow::Result<()> {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_elicitation(&server_name, &id)
                }
                None => None,
            }
        };
        if let Some(tx_response) = entry {
            tx_response
                .send(response)
                .map_err(|e| anyhow::anyhow!("failed to send elicitation response: {e:?}"))?;
            return Ok(());
        }

        self.services
            .mcp_registry
            .execute(&server_name.clone(), move |manager, server| async move {
                manager.resolve_elicitation(server, id, response).await
            })
            .await
    }

    pub(super) async fn refresh_mcp_servers_inner(
        &self,
        turn_context: &TurnContext,
        mcp_servers: HashMap<String, McpServerConfig>,
        store_mode: OAuthCredentialsStoreMode,
    ) -> anyhow::Result<McpServersRefreshedEvent> {
        let config = self.get_config().await;
        let auth_statuses = compute_auth_statuses(mcp_servers.iter(), store_mode).await;
        let permission_snapshot = self.permission_snapshot(turn_context).await;
        let sandbox_state = SandboxState {
            vfs_policy: permission_snapshot.effective_vfs_policy(),
            socket_policy: permission_snapshot.effective_socket_policy(),
            alcatraz_macos_exe: turn_context.alcatraz_macos_exe.clone(),
            alcatraz_linux_exe: turn_context.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: turn_context.alcatraz_freebsd_exe.clone(),
            sandbox_cwd: turn_context.cwd.clone(),
        };
        let approval_policy =
            chaos_sysctl::Constrained::allow_any(permission_snapshot.approval_policy);
        let client_identities = self
            .services
            .mcp_registry
            .client_identities_for(mcp_servers.keys().cloned());

        let mcp_catalog_gate = Arc::new(crate::catalog::McpCatalogGate::staging(Arc::clone(
            &self.services.catalog,
        )));
        let (refreshed_manager, cancel_token) = McpConnectionManager::new(
            &mcp_servers,
            &client_identities,
            store_mode,
            auth_statuses,
            &approval_policy,
            self.get_tx_event(),
            sandbox_state,
            config.chaos_home.clone(),
            Arc::clone(&mcp_catalog_gate) as Arc<dyn chaos_traits::McpCatalogSink>,
        )
        .await;
        let mut ready = Vec::new();
        let mut failed = Vec::new();
        for (server_name, server_config) in mcp_servers.iter().filter(|(_, cfg)| cfg.enabled) {
            let timeout = server_config
                .startup_timeout_sec
                .unwrap_or(chaos_mcp_runtime::manager::DEFAULT_STARTUP_TIMEOUT);
            if refreshed_manager
                .wait_for_server_ready(server_name, timeout)
                .await
            {
                ready.push(server_name.clone());
            } else {
                failed.push(McpStartupFailure {
                    server: server_name.clone(),
                    error: "server did not become ready during staged refresh".to_string(),
                });
            }
        }
        ready.sort();
        failed.sort_by(|a, b| a.server.cmp(&b.server));

        if !failed.is_empty() {
            cancel_token.cancel();
            refreshed_manager.shutdown().await.map_err(|error| {
                anyhow::anyhow!("failed to clean up rejected MCP refresh generation: {error:#}")
            })?;
            return Ok(McpServersRefreshedEvent {
                revision: self.services.mcp_registry.revision(),
                applied: false,
                added: Vec::new(),
                updated: Vec::new(),
                removed: Vec::new(),
                ready,
                failed,
            });
        }
        let mcp_tools = refreshed_manager.list_all_tools().await;
        let catalog_tools = mcp_tools
            .values()
            .map(|tool_info| {
                (
                    tool_info.server_name.clone(),
                    chaos_mcp_runtime::catalog_conv::mcp_tool_info_to_catalog_tool(tool_info),
                )
            })
            .collect();
        let diff = self
            .services
            .mcp_registry
            .reconcile(
                refreshed_manager,
                mcp_servers,
                cancel_token,
                mcp_catalog_gate,
                catalog_tools,
            )
            .await?;
        Ok(McpServersRefreshedEvent {
            revision: diff.revision,
            applied: true,
            added: diff.added,
            updated: diff.updated,
            removed: diff.removed,
            ready,
            failed,
        })
    }

    pub(super) async fn refresh_mcp_servers_now(
        &self,
        turn_context: &TurnContext,
        refresh_config: McpServerRefreshConfig,
    ) -> anyhow::Result<McpServersRefreshedEvent> {
        let McpServerRefreshConfig {
            mcp_servers,
            mcp_oauth_credentials_store_mode,
        } = refresh_config;

        let mcp_servers =
            match serde_json::from_value::<HashMap<String, McpServerConfig>>(mcp_servers) {
                Ok(servers) => servers,
                Err(err) => {
                    return Err(anyhow::anyhow!(
                        "failed to parse MCP server refresh config: {err}"
                    ));
                }
            };
        let store_mode = match serde_json::from_value::<OAuthCredentialsStoreMode>(
            mcp_oauth_credentials_store_mode,
        ) {
            Ok(mode) => mode,
            Err(err) => {
                return Err(anyhow::anyhow!(
                    "failed to parse MCP OAuth refresh config: {err}"
                ));
            }
        };

        self.refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await
    }

    #[cfg(test)]
    #[expect(dead_code, reason = "test helper available for future tests")]
    pub(super) async fn mcp_startup_cancellation_token(
        &self,
    ) -> tokio_util::sync::CancellationToken {
        self.services.mcp_registry.cancellation_token().await
    }

    pub(super) async fn cancel_mcp_startup(&self) {
        self.services
            .mcp_registry
            .cancel()
            .await
            .unwrap_or_else(|_| {
                panic!("MCP registry actor stopped while cancelling startup");
            });
    }
}
