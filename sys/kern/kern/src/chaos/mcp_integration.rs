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
use tokio_util::sync::CancellationToken;
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

impl Session {
    pub async fn list_resources(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> anyhow::Result<ListResourcesResult> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move { mgr.read().await.list_resources(server, params).await }
        })
        .await
    }

    pub async fn list_resource_templates(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> anyhow::Result<ListResourceTemplatesResult> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move {
                mgr.read()
                    .await
                    .list_resource_templates(server, params)
                    .await
            }
        })
        .await
    }

    pub async fn read_resource(
        &self,
        server: &str,
        params: ReadResourceRequestParams,
    ) -> anyhow::Result<ReadResourceResult> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move { mgr.read().await.read_resource(server, params).await }
        })
        .await
    }

    pub async fn call_tool_async(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
        ttl: Option<u64>,
    ) -> anyhow::Result<McpTask> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move {
                mgr.read()
                    .await
                    .call_tool_async(server, tool, arguments, meta, ttl)
                    .await
            }
        })
        .await
    }

    pub(crate) async fn get_mcp_task(
        &self,
        server: &str,
        task_id: &str,
    ) -> anyhow::Result<McpTask> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move { mgr.read().await.get_task(server, task_id).await }
        })
        .await
    }

    pub async fn get_mcp_task_result(
        &self,
        server: &str,
        task_id: &str,
    ) -> anyhow::Result<McpToolCallResult> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move { mgr.read().await.get_task_result(server, task_id).await }
        })
        .await
    }

    pub async fn list_mcp_tasks(&self, server: &str) -> anyhow::Result<ListTasksResult> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move { mgr.read().await.list_tasks(server).await }
        })
        .await
    }

    pub async fn cancel_mcp_task(&self, server: &str, task_id: &str) -> anyhow::Result<McpTask> {
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move { mgr.read().await.cancel_task(server, task_id).await }
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
        with_circuit_breaker(server, || {
            let mgr = self.services.mcp_connection_manager.clone();
            async move {
                mgr.read()
                    .await
                    .call_tool(server, tool, arguments, meta)
                    .await
            }
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
            .mcp_connection_manager
            .read()
            .await
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
            RequestId::Number(value) => chaos_ipc::mcp::RequestId::Integer(*value),
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
            .mcp_connection_manager
            .read()
            .await
            .resolve_elicitation(server_name, id, response)
            .await
    }

    pub(super) async fn refresh_mcp_servers_inner(
        &self,
        turn_context: &TurnContext,
        mcp_servers: HashMap<String, McpServerConfig>,
        store_mode: OAuthCredentialsStoreMode,
    ) {
        let config = self.get_config().await;
        let auth_statuses = compute_auth_statuses(mcp_servers.iter(), store_mode).await;
        let sandbox_state = SandboxState {
            file_system_sandbox_policy: turn_context.file_system_sandbox_policy.clone(),
            network_sandbox_policy: turn_context.network_sandbox_policy,
            alcatraz_macos_exe: turn_context.alcatraz_macos_exe.clone(),
            alcatraz_linux_exe: turn_context.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: turn_context.alcatraz_freebsd_exe.clone(),
            sandbox_cwd: turn_context.cwd.clone(),
        };
        {
            let mut guard = self.services.mcp_startup_cancellation_token.lock().await;
            guard.cancel();
            *guard = CancellationToken::new();
        }
        let (refreshed_manager, cancel_token) = McpConnectionManager::new(
            &mcp_servers,
            store_mode,
            auth_statuses,
            &turn_context.config.permissions.approval_policy,
            self.get_tx_event(),
            sandbox_state,
            config.chaos_home.clone(),
            Arc::clone(&self.services.catalog) as Arc<dyn chaos_traits::McpCatalogSink>,
        )
        .await;
        {
            let mut guard = self.services.mcp_startup_cancellation_token.lock().await;
            if guard.is_cancelled() {
                cancel_token.cancel();
            }
            *guard = cancel_token;
        }

        let mut manager = self.services.mcp_connection_manager.write().await;
        *manager = refreshed_manager;

        // Re-sync MCP tools into catalog after refresh.
        let mcp_tools = manager.list_all_tools().await;
        {
            let mut catalog = self
                .services
                .catalog
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Clear all previous MCP entries and re-register.
            catalog.clear_all_mcp();
            for tool_info in mcp_tools.values() {
                catalog.register_mcp_tools(
                    &tool_info.server_name,
                    vec![chaos_mcp_runtime::catalog_conv::mcp_tool_info_to_catalog_tool(tool_info)],
                );
            }
        }
    }

    pub(super) async fn refresh_mcp_servers_if_requested(&self, turn_context: &TurnContext) {
        let refresh_config = { self.pending_mcp_server_refresh_config.lock().await.take() };
        let Some(refresh_config) = refresh_config else {
            return;
        };

        let McpServerRefreshConfig {
            mcp_servers,
            mcp_oauth_credentials_store_mode,
        } = refresh_config;

        let mcp_servers =
            match serde_json::from_value::<HashMap<String, McpServerConfig>>(mcp_servers) {
                Ok(servers) => servers,
                Err(err) => {
                    warn!("failed to parse MCP server refresh config: {err}");
                    return;
                }
            };
        let store_mode = match serde_json::from_value::<OAuthCredentialsStoreMode>(
            mcp_oauth_credentials_store_mode,
        ) {
            Ok(mode) => mode,
            Err(err) => {
                warn!("failed to parse MCP OAuth refresh config: {err}");
                return;
            }
        };

        self.refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await;
    }

    #[cfg(test)]
    #[expect(dead_code, reason = "test helper available for future tests")]
    pub(super) async fn mcp_startup_cancellation_token(&self) -> CancellationToken {
        self.services
            .mcp_startup_cancellation_token
            .lock()
            .await
            .clone()
    }

    pub(super) async fn cancel_mcp_startup(&self) {
        self.services
            .mcp_startup_cancellation_token
            .lock()
            .await
            .cancel();
    }
}
