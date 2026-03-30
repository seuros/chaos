//! Connection manager for Model Context Protocol (MCP) servers.
//!
//! The [`McpConnectionManager`] owns one [`mcp_guest::McpSession`] per
//! configured server (keyed by the *server name*). It offers convenience
//! helpers to query the available tools across *all* servers and returns them
//! in a single aggregated map using the fully-qualified tool name
//! `"<server><MCP_TOOL_NAME_DELIMITER><tool>"` as the key.

use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use crate::mcp::auth::McpAuthStatusEntry;
use crate::mcp::oauth_types::OAuthCredentialsStoreMode;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use async_channel::Sender;
use chaos_epoll::CancelErr;
use chaos_epoll::OrCancelExt;
use chaos_ipc::approvals::ElicitationCompleteEvent;
use chaos_ipc::approvals::ElicitationRequest;
use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::mcp::CallToolResult;
use chaos_ipc::mcp::RequestId as ProtocolRequestId;
use chaos_ipc::protocol::AskForApproval;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::McpStartupCompleteEvent;
use chaos_ipc::protocol::McpStartupFailure;
use chaos_ipc::protocol::McpStartupStatus;
use chaos_ipc::protocol::McpStartupUpdateEvent;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_sysctl::Constrained;
use futures::future::BoxFuture;
use futures::future::FutureExt;
use futures::future::Shared;
use mcp_guest::ClientHandler;
use mcp_guest::ClientHandlerFuture;
use mcp_guest::ClientHandlerResultFuture;
use mcp_guest::McpSession;
use mcp_guest::protocol::CreateElicitationRequest;
use mcp_guest::protocol::CreateElicitationResponse;
use mcp_guest::protocol::ElicitationAction;
use mcp_guest::protocol::ElicitationCompleteNotificationParams;
use mcp_guest::protocol::ElicitationResponse;
use mcp_guest::protocol::RequestId;
use mcp_guest::protocol::TaskOrResult;
// Use mcp-guest types directly throughout core.
use mcp_guest::ListResourceTemplatesResult;
use mcp_guest::ListResourcesResult;
use mcp_guest::PaginatedRequestParams;
use mcp_guest::ReadResourceRequestParams;
use mcp_guest::ReadResourceResult;
use mcp_guest::ResourceInfo;
use mcp_guest::ResourceTemplateInfo;
pub(crate) use mcp_guest::ToolInfo as McpToolInfo;

use serde::Deserialize;
use serde::Serialize;
use sha1::Digest;
use sha1::Sha1;
use std::sync::RwLock as StdRwLock;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::warn;
use url::Url;

use crate::chaos::INITIAL_SUBMIT_ID;
use crate::config::types::McpServerConfig;
use crate::config::types::McpServerTransportConfig;

/// Delimiter used to separate the server name from the tool name in a fully
/// qualified tool name.
///
/// OpenAI requires tool names to conform to `^[a-zA-Z0-9_-]+$`, so we must
/// choose a delimiter from this character set.
const MCP_TOOL_NAME_DELIMITER: &str = "__";
const MAX_TOOL_NAME_LENGTH: usize = 64;

/// Default timeout for initializing MCP server & initially listing tools.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Default timeout for individual tool calls.
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

const MCP_TOOLS_FETCH_UNCACHED_DURATION_METRIC: &str = "codex.mcp.tools.fetch_uncached.duration_ms";
const MIN_COMPATIBLE_MCP_CLIENT_VERSION: &str = "0.63.0";

/// Default env vars inherited by stdio MCP server processes.
const DEFAULT_ENV_VARS: &[&str] = &[
    "HOME",
    "LOGNAME",
    "PATH",
    "SHELL",
    "USER",
    "__CF_USER_TEXT_ENCODING",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "TZ",
];

/// The Responses API requires tool names to match `^[a-zA-Z0-9_-]+$`.
/// MCP server/tool names are user-controlled, so sanitize the fully-qualified
/// name we expose to the model by replacing any disallowed character with `_`.
fn sanitize_responses_api_tool_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            sanitized.push(c);
        } else {
            sanitized.push('_');
        }
    }

    if sanitized.is_empty() {
        "_".to_string()
    } else {
        sanitized
    }
}

fn sha1_hex(s: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(s.as_bytes());
    let sha1 = hasher.finalize();
    digest_hex(&sha1)
}

fn digest_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn mcp_client_implementation_version() -> &'static str {
    let version = env!("CARGO_PKG_VERSION");
    if version == "0.0.0" {
        MIN_COMPATIBLE_MCP_CLIENT_VERSION
    } else {
        version
    }
}

fn qualify_tools<I>(tools: I) -> HashMap<String, ToolInfo>
where
    I: IntoIterator<Item = ToolInfo>,
{
    let mut used_names = HashSet::new();
    let mut seen_raw_names = HashSet::new();
    let mut qualified_tools = HashMap::new();
    for tool in tools {
        let qualified_name_raw = format!(
            "mcp{}{}{}{}",
            MCP_TOOL_NAME_DELIMITER, tool.server_name, MCP_TOOL_NAME_DELIMITER, tool.tool_name
        );
        if !seen_raw_names.insert(qualified_name_raw.clone()) {
            warn!("skipping duplicated tool {}", qualified_name_raw);
            continue;
        }

        // Start from a "pretty" name (sanitized), then deterministically disambiguate on
        // collisions by appending a hash of the *raw* (unsanitized) qualified name. This
        // ensures tools like `foo.bar` and `foo_bar` don't collapse to the same key.
        let mut qualified_name = sanitize_responses_api_tool_name(&qualified_name_raw);

        // Enforce length constraints early; use the raw name for the hash input so the
        // output remains stable even when sanitization changes.
        if qualified_name.len() > MAX_TOOL_NAME_LENGTH {
            let sha1_str = sha1_hex(&qualified_name_raw);
            let prefix_len = MAX_TOOL_NAME_LENGTH - sha1_str.len();
            qualified_name = format!("{}{}", &qualified_name[..prefix_len], sha1_str);
        }

        if used_names.contains(&qualified_name) {
            warn!("skipping duplicated tool {}", qualified_name);
            continue;
        }

        used_names.insert(qualified_name.clone());
        qualified_tools.insert(qualified_name, tool);
    }

    qualified_tools
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolInfo {
    pub(crate) server_name: String,
    pub(crate) tool_name: String,
    pub(crate) tool_namespace: String,
    pub(crate) tool: McpToolInfo,
    pub(crate) connector_id: Option<String>,
    pub(crate) connector_name: Option<String>,
    pub(crate) connector_description: Option<String>,
}

type ResponderMap = HashMap<(String, RequestId), oneshot::Sender<ElicitationResponse>>;

fn elicitation_is_rejected_by_policy(approval_policy: AskForApproval) -> bool {
    match approval_policy {
        AskForApproval::Never => true,
        AskForApproval::OnFailure => false,
        AskForApproval::OnRequest => false,
        AskForApproval::UnlessTrusted => false,
        AskForApproval::Granular(granular_config) => !granular_config.allows_mcp_elicitations(),
    }
}

#[derive(Clone)]
struct ElicitationRequestManager {
    requests: Arc<Mutex<ResponderMap>>,
    approval_policy: Arc<StdMutex<AskForApproval>>,
}

impl ElicitationRequestManager {
    fn new(approval_policy: AskForApproval) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            approval_policy: Arc::new(StdMutex::new(approval_policy)),
        }
    }

    async fn resolve(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> Result<()> {
        self.requests
            .lock()
            .await
            .remove(&(server_name, id))
            .ok_or_else(|| anyhow!("elicitation request not found"))?
            .send(response)
            .map_err(|e| anyhow!("failed to send elicitation response: {e:?}"))
    }
}

/// Handler that bridges mcp-guest callbacks to the core event system.
struct ChaosClientHandler {
    server_name: String,
    tx_event: Sender<Event>,
    elicitation_requests: ElicitationRequestManager,
    /// Shared tool store + filter for refreshing on list_changed.
    tools_arc: Arc<StdRwLock<Vec<ToolInfo>>>,
    tool_filter: ToolFilter,
    tool_timeout: Duration,
    /// The session is set after connect. We need it for re-listing tools
    /// when the server sends a tools/list_changed notification.
    session: Arc<tokio::sync::RwLock<Option<McpSession>>>,
    /// Shared catalog for updating on list_changed notifications.
    catalog: Arc<StdRwLock<crate::catalog::Catalog>>,
}

impl ClientHandler for ChaosClientHandler {
    fn create_elicitation(
        &self,
        request: CreateElicitationRequest,
    ) -> ClientHandlerResultFuture<'_, CreateElicitationResponse> {
        Box::pin(async move {
            if self
                .elicitation_requests
                .approval_policy
                .lock()
                .is_ok_and(|policy| elicitation_is_rejected_by_policy(*policy))
            {
                return Ok(TaskOrResult::Result(
                    mcp_guest::protocol::CreateElicitationResult {
                        action: ElicitationAction::Decline,
                        content: None,
                    },
                ));
            }

            let elicitation_request = match &request {
                CreateElicitationRequest::Form(form) => ElicitationRequest::Form {
                    meta: None,
                    message: form.message.clone(),
                    requested_schema: form.requested_schema.clone(),
                },
                CreateElicitationRequest::Url(url_req) => ElicitationRequest::Url {
                    meta: None,
                    message: url_req.message.clone(),
                    url: url_req.url.clone(),
                    elicitation_id: url_req.elicitation_id.clone(),
                },
            };

            // Generate a request ID for tracking.
            let id = RequestId::number(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
            );

            let (tx, rx) = oneshot::channel();
            {
                let mut lock = self.elicitation_requests.requests.lock().await;
                lock.insert((self.server_name.clone(), id.clone()), tx);
            }
            let _ = self
                .tx_event
                .send(Event {
                    id: "mcp_elicitation_request".to_string(),
                    msg: EventMsg::ElicitationRequest(ElicitationRequestEvent {
                        turn_id: None,
                        server_name: self.server_name.clone(),
                        id: request_id_to_protocol(&id),
                        request: elicitation_request,
                    }),
                })
                .await;

            let response = rx.await.map_err(|_| mcp_guest::GuestError::Disconnected)?;

            Ok(TaskOrResult::Result(
                mcp_guest::protocol::CreateElicitationResult {
                    action: response.action,
                    content: response.content,
                },
            ))
        })
    }

    fn on_tools_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async move {
            let session_guard = self.session.read().await;
            let Some(session) = session_guard.as_ref() else {
                return;
            };

            match list_tools_for_session_uncached(
                &self.server_name,
                session,
                Some(self.tool_timeout),
            )
            .await
            {
                Ok(tools) => {
                    // Update the per-server tool store (used by ToolRouter).
                    store_managed_tools(&self.tool_filter, &self.tools_arc, tools);

                    // Sync to catalog: drop old entries, re-register from the refreshed store.
                    if let Ok(store) = self.tools_arc.read() {
                        let catalog_tools: Vec<_> = store
                            .iter()
                            .map(crate::catalog::mcp_tool_info_to_catalog_tool)
                            .collect();
                        if let Ok(mut catalog) = self.catalog.write() {
                            catalog.unregister_mcp(&self.server_name);
                            catalog.register_mcp_tools(&self.server_name, catalog_tools);
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        "Failed to refresh tool list for '{}': {err}",
                        self.server_name
                    );
                }
            }
        })
    }

    fn on_resources_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async move {
            let session_guard = self.session.read().await;
            let Some(session) = session_guard.as_ref() else {
                return;
            };

            // Re-list resources and resource templates from the server.
            let resources = match session.list_resources().await {
                Ok(list) => list
                    .iter()
                    .map(crate::catalog::mcp_resource_to_catalog)
                    .collect(),
                Err(err) => {
                    warn!(
                        "Failed to refresh resource list for '{}': {err}",
                        self.server_name
                    );
                    return;
                }
            };

            let templates = match session.list_resource_templates().await {
                Ok(list) => list
                    .iter()
                    .map(crate::catalog::mcp_resource_template_to_catalog)
                    .collect(),
                Err(err) => {
                    warn!(
                        "Failed to refresh resource template list for '{}': {err}",
                        self.server_name
                    );
                    Vec::new()
                }
            };

            if let Ok(mut catalog) = self.catalog.write() {
                // Unregister clears tools+resources+templates+prompts for the server,
                // so we only clear resources/templates selectively here.
                catalog.unregister_mcp_resources(&self.server_name);
                catalog.register_mcp_resources(&self.server_name, resources);
                catalog.register_mcp_resource_templates(&self.server_name, templates);
            }
        })
    }

    fn on_prompts_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async move {
            let session_guard = self.session.read().await;
            let Some(session) = session_guard.as_ref() else {
                return;
            };

            let prompts = match session.list_prompts().await {
                Ok(result) => result
                    .iter()
                    .map(crate::catalog::mcp_prompt_to_catalog)
                    .collect(),
                Err(err) => {
                    warn!(
                        "Failed to refresh prompt list for '{}': {err}",
                        self.server_name
                    );
                    return;
                }
            };

            if let Ok(mut catalog) = self.catalog.write() {
                catalog.unregister_mcp_prompts(&self.server_name);
                catalog.register_mcp_prompts(&self.server_name, prompts);
            }
        })
    }

    fn on_elicitation_complete(
        &self,
        params: ElicitationCompleteNotificationParams,
    ) -> ClientHandlerFuture<'_> {
        let tx_event = self.tx_event.clone();
        let server_name = self.server_name.clone();
        Box::pin(async move {
            let _ = tx_event
                .send(Event {
                    id: "mcp_elicitation_complete".to_string(),
                    msg: EventMsg::ElicitationComplete(ElicitationCompleteEvent {
                        server_name,
                        elicitation_id: params.elicitation_id,
                    }),
                })
                .await;
        })
    }
}

/// Convert mcp-guest RequestId to protocol RequestId.
fn request_id_to_protocol(id: &RequestId) -> ProtocolRequestId {
    match id {
        RequestId::String(s) => ProtocolRequestId::String(s.clone()),
        RequestId::Number(n) => ProtocolRequestId::Integer(*n),
    }
}

/// Convert protocol RequestId to mcp-guest RequestId.
pub(crate) fn protocol_request_id_to_guest(id: &ProtocolRequestId) -> RequestId {
    match id {
        ProtocolRequestId::String(s) => RequestId::string(s.clone()),
        ProtocolRequestId::Integer(n) => RequestId::number(*n),
    }
}

#[derive(Clone)]
struct ManagedClient {
    session: McpSession,
    tools: Arc<StdRwLock<Vec<ToolInfo>>>,
    tool_filter: ToolFilter,
    _tool_timeout: Option<Duration>,
}

impl ManagedClient {
    fn listed_tools(&self) -> Vec<ToolInfo> {
        let in_memory_tools = self
            .tools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        filter_tools(in_memory_tools, &self.tool_filter)
    }

    /// Sends sandbox state as a standard MCP log notification.
    async fn notify_sandbox_state_change(&self, sandbox_state: &SandboxState) -> Result<()> {
        self.session
            .notify_value(
                "notifications/message",
                Some(serde_json::json!({
                    "level": "info",
                    "logger": "chaos/alcatraz-state",
                    "data": serde_json::to_value(sandbox_state)?,
                })),
            )
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }
}

#[derive(Clone)]
struct AsyncManagedClient {
    client: Shared<BoxFuture<'static, Result<ManagedClient, StartupOutcomeError>>>,
    startup_snapshot: Option<Vec<ToolInfo>>,
    startup_complete: Arc<AtomicBool>,
}

impl AsyncManagedClient {
    #[allow(clippy::too_many_arguments)]
    fn new(
        server_name: String,
        config: McpServerConfig,
        _store_mode: OAuthCredentialsStoreMode,
        cancel_token: CancellationToken,
        tx_event: Sender<Event>,
        elicitation_requests: ElicitationRequestManager,
        catalog: Arc<StdRwLock<crate::catalog::Catalog>>,
    ) -> Self {
        let tool_filter = ToolFilter::from_config(&config);
        let startup_tool_filter = tool_filter;
        let startup_complete = Arc::new(AtomicBool::new(false));
        let startup_complete_for_fut = Arc::clone(&startup_complete);
        let fut = async move {
            let outcome = async {
                if let Err(error) = validate_mcp_server_name(&server_name) {
                    return Err(error.into());
                }

                make_managed_client(
                    server_name,
                    config,
                    MakeClientParams {
                        tool_filter: startup_tool_filter,
                        tx_event,
                        elicitation_requests,
                        catalog,
                    },
                )
                .or_cancel(&cancel_token)
                .await
                .map_err(|CancelErr::Cancelled| StartupOutcomeError::Cancelled)?
            }
            .await;

            startup_complete_for_fut.store(true, Ordering::Release);
            outcome
        };
        let client = fut.boxed().shared();

        Self {
            client,
            startup_snapshot: None,
            startup_complete,
        }
    }

    async fn client(&self) -> Result<ManagedClient, StartupOutcomeError> {
        self.client.clone().await
    }

    fn startup_snapshot_while_initializing(&self) -> Option<Vec<ToolInfo>> {
        if !self.startup_complete.load(Ordering::Acquire) {
            return self.startup_snapshot.clone();
        }
        None
    }

    async fn listed_tools(&self) -> Option<Vec<ToolInfo>> {
        if let Some(startup_tools) = self.startup_snapshot_while_initializing() {
            Some(startup_tools)
        } else {
            match self.client().await {
                Ok(client) => Some(client.listed_tools()),
                Err(_) => self.startup_snapshot.clone(),
            }
        }
    }

    async fn notify_sandbox_state_change(&self, sandbox_state: &SandboxState) -> Result<()> {
        let managed = self.client().await?;
        managed.notify_sandbox_state_change(sandbox_state).await
    }
}

/// Logger name used to identify sandbox state notifications.
pub const MCP_SANDBOX_STATE_LOGGER: &str = "chaos/alcatraz-state";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxState {
    pub sandbox_policy: SandboxPolicy,
    pub alcatraz_macos_exe: Option<PathBuf>,
    pub alcatraz_linux_exe: Option<PathBuf>,
    pub alcatraz_freebsd_exe: Option<PathBuf>,
    pub sandbox_cwd: PathBuf,
}

/// A thin wrapper around a set of running [`McpSession`] instances.
pub(crate) struct McpConnectionManager {
    clients: HashMap<String, AsyncManagedClient>,
    server_origins: HashMap<String, String>,
    elicitation_requests: ElicitationRequestManager,
}

impl McpConnectionManager {
    pub(crate) fn new_uninitialized(approval_policy: &Constrained<AskForApproval>) -> Self {
        Self {
            clients: HashMap::new(),
            server_origins: HashMap::new(),
            elicitation_requests: ElicitationRequestManager::new(approval_policy.value()),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_mcp_connection_manager_for_tests(
        approval_policy: &Constrained<AskForApproval>,
    ) -> Self {
        Self::new_uninitialized(approval_policy)
    }

    pub(crate) fn has_servers(&self) -> bool {
        !self.clients.is_empty()
    }

    pub(crate) fn server_origin(&self, server_name: &str) -> Option<&str> {
        self.server_origins.get(server_name).map(String::as_str)
    }

    pub fn set_approval_policy(&self, approval_policy: &Constrained<AskForApproval>) {
        if let Ok(mut policy) = self.elicitation_requests.approval_policy.lock() {
            *policy = approval_policy.value();
        }
    }

    #[allow(clippy::new_ret_no_self, clippy::too_many_arguments)]
    pub async fn new(
        mcp_servers: &HashMap<String, McpServerConfig>,
        store_mode: OAuthCredentialsStoreMode,
        auth_entries: HashMap<String, McpAuthStatusEntry>,
        approval_policy: &Constrained<AskForApproval>,
        tx_event: Sender<Event>,
        initial_sandbox_state: SandboxState,
        _codex_home: PathBuf,
        catalog: Arc<StdRwLock<crate::catalog::Catalog>>,
    ) -> (Self, CancellationToken) {
        let cancel_token = CancellationToken::new();
        let mut clients = HashMap::new();
        let mut server_origins = HashMap::new();
        let mut join_set = JoinSet::new();
        let elicitation_requests = ElicitationRequestManager::new(approval_policy.value());
        let mcp_servers = mcp_servers.clone();
        for (server_name, cfg) in mcp_servers.into_iter().filter(|(_, cfg)| cfg.enabled) {
            if let Some(origin) = transport_origin(&cfg.transport) {
                server_origins.insert(server_name.clone(), origin);
            }
            let cancel_token = cancel_token.child_token();
            let _ = emit_update(
                &tx_event,
                McpStartupUpdateEvent {
                    server: server_name.clone(),
                    status: McpStartupStatus::Starting,
                },
            )
            .await;
            let async_managed_client = AsyncManagedClient::new(
                server_name.clone(),
                cfg,
                store_mode,
                cancel_token.clone(),
                tx_event.clone(),
                elicitation_requests.clone(),
                Arc::clone(&catalog),
            );
            clients.insert(server_name.clone(), async_managed_client.clone());
            let tx_event = tx_event.clone();
            let auth_entry = auth_entries.get(&server_name).cloned();
            let sandbox_state = initial_sandbox_state.clone();
            join_set.spawn(async move {
                let outcome = async_managed_client.client().await;
                if cancel_token.is_cancelled() {
                    return (server_name, Err(StartupOutcomeError::Cancelled));
                }
                let status = match &outcome {
                    Ok(_) => {
                        // Send sandbox state notification immediately after Ready
                        if let Err(e) = async_managed_client
                            .notify_sandbox_state_change(&sandbox_state)
                            .await
                        {
                            warn!(
                                "Failed to notify sandbox state to MCP server {server_name}: {e:#}",
                            );
                        }
                        McpStartupStatus::Ready
                    }
                    Err(error) => {
                        let error_str = mcp_init_error_display(
                            server_name.as_str(),
                            auth_entry.as_ref(),
                            error,
                        );
                        McpStartupStatus::Failed { error: error_str }
                    }
                };

                let _ = emit_update(
                    &tx_event,
                    McpStartupUpdateEvent {
                        server: server_name.clone(),
                        status,
                    },
                )
                .await;

                (server_name, outcome)
            });
        }
        let manager = Self {
            clients,
            server_origins,
            elicitation_requests: elicitation_requests.clone(),
        };
        tokio::spawn(async move {
            let outcomes = join_set.join_all().await;
            let mut summary = McpStartupCompleteEvent::default();
            for (server_name, outcome) in outcomes {
                match outcome {
                    Ok(_) => summary.ready.push(server_name),
                    Err(StartupOutcomeError::Cancelled) => summary.cancelled.push(server_name),
                    Err(StartupOutcomeError::Failed { error }) => {
                        summary.failed.push(McpStartupFailure {
                            server: server_name,
                            error,
                        })
                    }
                }
            }
            let _ = tx_event
                .send(Event {
                    id: INITIAL_SUBMIT_ID.to_owned(),
                    msg: EventMsg::McpStartupComplete(summary),
                })
                .await;
        });
        (manager, cancel_token)
    }

    async fn client_by_name(&self, name: &str) -> Result<ManagedClient> {
        self.clients
            .get(name)
            .ok_or_else(|| anyhow!("unknown MCP server '{name}'"))?
            .client()
            .await
            .context("failed to get client")
    }

    pub async fn resolve_elicitation(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> Result<()> {
        self.elicitation_requests
            .resolve(server_name, id, response)
            .await
    }

    #[allow(dead_code)]
    pub(crate) async fn wait_for_server_ready(&self, server_name: &str, timeout: Duration) -> bool {
        let Some(async_managed_client) = self.clients.get(server_name) else {
            return false;
        };

        match tokio::time::timeout(timeout, async_managed_client.client()).await {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        }
    }

    pub(crate) async fn required_startup_failures(
        &self,
        required_servers: &[String],
    ) -> Vec<McpStartupFailure> {
        let mut failures = Vec::new();
        for server_name in required_servers {
            let Some(async_managed_client) = self.clients.get(server_name).cloned() else {
                failures.push(McpStartupFailure {
                    server: server_name.clone(),
                    error: format!("required MCP server `{server_name}` was not initialized"),
                });
                continue;
            };

            match async_managed_client.client().await {
                Ok(_) => {}
                Err(error) => failures.push(McpStartupFailure {
                    server: server_name.clone(),
                    error: startup_outcome_error_message(error),
                }),
            }
        }
        failures
    }

    /// Returns a single map that contains all tools. Each key is the
    /// fully-qualified name for the tool.
    #[instrument(level = "trace", skip_all)]
    pub async fn list_all_tools(&self) -> HashMap<String, ToolInfo> {
        let mut tools = HashMap::new();
        for managed_client in self.clients.values() {
            let Some(server_tools) = managed_client.listed_tools().await else {
                continue;
            };
            tools.extend(qualify_tools(server_tools));
        }
        tools
    }

    /// Returns a single map that contains all resources. Each key is the
    /// server name and the value is a vector of resources.
    pub async fn list_all_resources(&self) -> HashMap<String, Vec<ResourceInfo>> {
        let mut join_set = JoinSet::new();

        let clients_snapshot = &self.clients;

        for (server_name, async_managed_client) in clients_snapshot {
            let server_name = server_name.clone();
            let Ok(managed_client) = async_managed_client.client().await else {
                continue;
            };
            let session = managed_client.session.clone();

            join_set.spawn(async move {
                match session.list_resources().await {
                    Ok(resources) => (server_name, Ok(resources)),
                    Err(err) => (server_name, Err(anyhow!("{err}"))),
                }
            });
        }

        let mut aggregated: HashMap<String, Vec<ResourceInfo>> = HashMap::new();

        while let Some(join_res) = join_set.join_next().await {
            match join_res {
                Ok((server_name, Ok(resources))) => {
                    aggregated.insert(server_name, resources);
                }
                Ok((server_name, Err(err))) => {
                    warn!("Failed to list resources for MCP server '{server_name}': {err:#}");
                }
                Err(err) => {
                    warn!("Task panic when listing resources for MCP server: {err:#}");
                }
            }
        }

        aggregated
    }

    /// Returns a single map that contains all resource templates. Each key is the
    /// server name and the value is a vector of resource templates.
    pub async fn list_all_resource_templates(&self) -> HashMap<String, Vec<ResourceTemplateInfo>> {
        let mut join_set = JoinSet::new();

        let clients_snapshot = &self.clients;

        for (server_name, async_managed_client) in clients_snapshot {
            let server_name_cloned = server_name.clone();
            let Ok(managed_client) = async_managed_client.client().await else {
                continue;
            };
            let session = managed_client.session.clone();

            join_set.spawn(async move {
                match session.list_resource_templates().await {
                    Ok(templates) => (server_name_cloned, Ok(templates)),
                    Err(err) => (server_name_cloned, Err(anyhow!("{err}"))),
                }
            });
        }

        let mut aggregated: HashMap<String, Vec<ResourceTemplateInfo>> = HashMap::new();

        while let Some(join_res) = join_set.join_next().await {
            match join_res {
                Ok((server_name, Ok(templates))) => {
                    aggregated.insert(server_name, templates);
                }
                Ok((server_name, Err(err))) => {
                    warn!(
                        "Failed to list resource templates for MCP server '{server_name}': {err:#}"
                    );
                }
                Err(err) => {
                    warn!("Task panic when listing resource templates for MCP server: {err:#}");
                }
            }
        }

        aggregated
    }

    /// Invoke the tool indicated by the (server, tool) pair.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
    ) -> Result<CallToolResult> {
        let client = self.client_by_name(server).await?;
        if !client.tool_filter.allows(tool) {
            return Err(anyhow!(
                "tool '{tool}' is disabled for MCP server '{server}'"
            ));
        }

        // Convert arguments Value to Map<String, Value> for mcp-guest
        let arguments_map = arguments.and_then(|v| match v {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        });

        let params = mcp_guest::protocol::CallToolRequestParams {
            name: tool.to_string(),
            arguments: arguments_map,
            meta,
            task: None,
        };

        let response = client
            .session
            .call_tool_with(params)
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("tool call failed for `{server}/{tool}`"))?;

        // Extract the result from TaskOrResult (we only handle direct results for now)
        let result = match response {
            TaskOrResult::Result(result) => result,
            TaskOrResult::Task(task_result) => {
                return Err(anyhow!(
                    "tool call returned async task (id: {}), which is not yet supported",
                    task_result.task.task_id
                ));
            }
        };

        let content = result
            .content
            .into_iter()
            .map(|content| {
                serde_json::to_value(content)
                    .unwrap_or_else(|_| serde_json::Value::String("<content>".to_string()))
            })
            .collect();

        Ok(CallToolResult {
            content,
            structured_content: result.structured_content,
            is_error: result.is_error,
            meta: result.meta,
        })
    }

    /// List resources from the specified server.
    pub async fn list_resources(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> Result<ListResourcesResult> {
        let managed = self.client_by_name(server).await?;

        let guest_params = params.unwrap_or(PaginatedRequestParams { cursor: None });

        let result: ListResourcesResult = managed
            .session
            .request("resources/list", &guest_params)
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("resources/list failed for `{server}`"))?;

        Ok(result)
    }

    /// List resource templates from the specified server.
    pub async fn list_resource_templates(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> Result<ListResourceTemplatesResult> {
        let managed = self.client_by_name(server).await?;

        let guest_params = params.unwrap_or(PaginatedRequestParams { cursor: None });

        let result: ListResourceTemplatesResult = managed
            .session
            .request("resources/templates/list", &guest_params)
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("resources/templates/list failed for `{server}`"))?;

        Ok(result)
    }

    /// Read a resource from the specified server.
    pub async fn read_resource(
        &self,
        server: &str,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult> {
        let managed = self.client_by_name(server).await?;
        let uri = params.uri.clone();

        let result: ReadResourceResult = managed
            .session
            .request("resources/read", &params)
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("resources/read failed for `{server}` ({uri})"))?;

        Ok(result)
    }

    pub async fn parse_tool_name(&self, tool_name: &str) -> Option<(String, String)> {
        self.list_all_tools()
            .await
            .get(tool_name)
            .map(|tool| (tool.server_name.clone(), tool.tool.name.to_string()))
    }

    pub async fn notify_sandbox_state_change(&self, sandbox_state: &SandboxState) -> Result<()> {
        let mut join_set = JoinSet::new();

        for async_managed_client in self.clients.values() {
            let sandbox_state = sandbox_state.clone();
            let async_managed_client = async_managed_client.clone();
            join_set.spawn(async move {
                async_managed_client
                    .notify_sandbox_state_change(&sandbox_state)
                    .await
            });
        }

        while let Some(join_res) = join_set.join_next().await {
            match join_res {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    warn!("Failed to notify sandbox state change to MCP server: {err:#}");
                }
                Err(err) => {
                    warn!("Task panic when notifying sandbox state change to MCP server: {err:#}");
                }
            }
        }

        Ok(())
    }
}

async fn emit_update(
    tx_event: &Sender<Event>,
    update: McpStartupUpdateEvent,
) -> Result<(), async_channel::SendError<Event>> {
    tx_event
        .send(Event {
            id: INITIAL_SUBMIT_ID.to_owned(),
            msg: EventMsg::McpStartupUpdate(update),
        })
        .await
}

/// A tool is allowed to be used if both are true:
/// 1. enabled is None (no allowlist is set) or the tool is explicitly enabled.
/// 2. The tool is not explicitly disabled.
#[derive(Default, Clone)]
pub(crate) struct ToolFilter {
    enabled: Option<HashSet<String>>,
    disabled: HashSet<String>,
}

impl ToolFilter {
    fn from_config(cfg: &McpServerConfig) -> Self {
        let enabled = cfg
            .enabled_tools
            .as_ref()
            .map(|tools| tools.iter().cloned().collect::<HashSet<_>>());
        let disabled = cfg
            .disabled_tools
            .as_ref()
            .map(|tools| tools.iter().cloned().collect::<HashSet<_>>())
            .unwrap_or_default();

        Self { enabled, disabled }
    }

    fn allows(&self, tool_name: &str) -> bool {
        if let Some(enabled) = &self.enabled
            && !enabled.contains(tool_name)
        {
            return false;
        }

        !self.disabled.contains(tool_name)
    }
}

fn filter_tools(tools: Vec<ToolInfo>, filter: &ToolFilter) -> Vec<ToolInfo> {
    tools
        .into_iter()
        .filter(|tool| filter.allows(&tool.tool.name))
        .collect()
}

fn emit_duration(metric: &str, duration: Duration, tags: &[(&str, &str)]) {
    if let Some(metrics) = chaos_syslog::metrics::global() {
        let _ = metrics.record_duration(metric, duration, tags);
    }
}

fn transport_origin(transport: &McpServerTransportConfig) -> Option<String> {
    match transport {
        McpServerTransportConfig::StreamableHttp { url, .. } => {
            let parsed = Url::parse(url).ok()?;
            Some(parsed.origin().ascii_serialization())
        }
        McpServerTransportConfig::Stdio { .. } => Some("stdio".to_string()),
    }
}

fn resolve_bearer_token(
    server_name: &str,
    bearer_token_env_var: Option<&str>,
) -> Result<Option<String>> {
    let Some(env_var) = bearer_token_env_var else {
        return Ok(None);
    };

    match env::var(env_var) {
        Ok(value) => {
            if value.is_empty() {
                Err(anyhow!(
                    "Environment variable {env_var} for MCP server '{server_name}' is empty"
                ))
            } else {
                Ok(Some(value))
            }
        }
        Err(env::VarError::NotPresent) => Err(anyhow!(
            "Environment variable {env_var} for MCP server '{server_name}' is not set"
        )),
        Err(env::VarError::NotUnicode(_)) => Err(anyhow!(
            "Environment variable {env_var} for MCP server '{server_name}' contains invalid Unicode"
        )),
    }
}

#[derive(Debug, Clone, thiserror::Error)]
enum StartupOutcomeError {
    #[error("MCP startup cancelled")]
    Cancelled,
    // We can't store the original error here because anyhow::Error doesn't implement
    // `Clone`.
    #[error("MCP startup failed: {error}")]
    Failed { error: String },
}

impl From<anyhow::Error> for StartupOutcomeError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed {
            error: error.to_string(),
        }
    }
}

struct MakeClientParams {
    tool_filter: ToolFilter,
    tx_event: Sender<Event>,
    elicitation_requests: ElicitationRequestManager,
    catalog: Arc<StdRwLock<crate::catalog::Catalog>>,
}

/// Build an env HashMap for a stdio MCP server child process.
fn create_env_for_mcp_server(
    extra_env: Option<HashMap<String, String>>,
    env_vars: &[String],
) -> HashMap<String, String> {
    DEFAULT_ENV_VARS
        .iter()
        .copied()
        .chain(env_vars.iter().map(String::as_str))
        .filter_map(|var| env::var(var).ok().map(|value| (var.to_string(), value)))
        .chain(extra_env.unwrap_or_default())
        .collect()
}

/// Resolve env_http_headers from environment variables and merge with static headers.
fn resolve_http_headers(
    http_headers: Option<HashMap<String, String>>,
    env_http_headers: Option<HashMap<String, String>>,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();

    if let Some(static_headers) = http_headers {
        for (name, value) in static_headers {
            headers.push((name, value));
        }
    }

    if let Some(env_headers) = env_http_headers {
        for (name, env_var) in env_headers {
            if let Ok(value) = env::var(&env_var) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    headers.push((name, value));
                }
            }
        }
    }

    headers
}

/// Build a McpSession via mcp-guest and return a fully-initialized ManagedClient.
async fn make_managed_client(
    server_name: String,
    config: McpServerConfig,
    params: MakeClientParams,
) -> Result<ManagedClient, StartupOutcomeError> {
    let MakeClientParams {
        tool_filter,
        tx_event,
        elicitation_requests,
        catalog,
    } = params;

    let tool_timeout = config.tool_timeout_sec.unwrap_or(DEFAULT_TOOL_TIMEOUT);
    let startup_timeout = config.startup_timeout_sec.or(Some(DEFAULT_STARTUP_TIMEOUT));

    let tools_arc: Arc<StdRwLock<Vec<ToolInfo>>> = Arc::new(StdRwLock::new(Vec::new()));
    let session_holder: Arc<tokio::sync::RwLock<Option<McpSession>>> =
        Arc::new(tokio::sync::RwLock::new(None));

    let handler = ChaosClientHandler {
        server_name: server_name.clone(),
        tx_event,
        elicitation_requests,
        tools_arc: Arc::clone(&tools_arc),
        tool_filter: tool_filter.clone(),
        tool_timeout,
        session: Arc::clone(&session_holder),
        catalog,
    };

    let client_info = mcp_guest::protocol::Implementation::new(
        "chaos-mcp-client",
        mcp_client_implementation_version(),
    )
    .with_title("ChaOS");

    let capabilities = mcp_guest::protocol::ClientCapabilities {
        experimental: None,
        roots: Some(mcp_guest::protocol::RootsCapability {
            list_changed: Some(false),
        }),
        sampling: Some(mcp_guest::protocol::SamplingCapability {
            context: None,
            tools: None,
        }),
        elicitation: Some(mcp_guest::protocol::ElicitationCapability {
            form: Some(mcp_guest::protocol::FormElicitationCapability {}),
            url: Some(mcp_guest::protocol::UrlElicitationCapability {}),
        }),
        tasks: None,
    };

    // Build and connect session based on transport type
    let connect_fut = async {
        match config.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                env,
                env_vars,
                cwd,
            } => {
                let envs = create_env_for_mcp_server(env, &env_vars);
                let mut builder = mcp_guest::stdio(&command, &args)
                    .envs(&envs)
                    .client_info(client_info)
                    .capabilities(capabilities)
                    .handler(handler)
                    .request_timeout(tool_timeout);

                if let Some(cwd_path) = cwd {
                    builder = builder.cwd(cwd_path);
                }

                builder.connect().await.map_err(|e| anyhow!("{e}"))
            }
            McpServerTransportConfig::StreamableHttp {
                url,
                http_headers,
                env_http_headers,
                bearer_token_env_var,
            } => {
                let resolved_bearer_token =
                    resolve_bearer_token(&server_name, bearer_token_env_var.as_deref())?;

                let resolved_headers = resolve_http_headers(http_headers, env_http_headers);

                let mut builder = mcp_guest::http(&url)
                    .headers(resolved_headers)
                    .client_info(client_info)
                    .capabilities(capabilities)
                    .handler(handler)
                    .request_timeout(tool_timeout);

                if let Some(token) = resolved_bearer_token {
                    builder = builder.bearer_token(token);
                }

                builder.connect().await.map_err(|e| anyhow!("{e}"))
            }
        }
    };

    // Wrap with startup timeout
    let session = if let Some(timeout) = startup_timeout {
        match tokio::time::timeout(timeout, connect_fut).await {
            Ok(result) => result.map_err(StartupOutcomeError::from)?,
            Err(_) => {
                return Err(StartupOutcomeError::Failed {
                    error: "timed out handshaking with MCP server".to_string(),
                });
            }
        }
    } else {
        connect_fut.await.map_err(StartupOutcomeError::from)?
    };

    // Store session in handler's session holder so tools_list_changed can use it
    *session_holder.write().await = Some(session.clone());

    // List tools
    let fetch_start = Instant::now();
    let tools = list_tools_for_session_uncached(&server_name, &session, startup_timeout)
        .await
        .map_err(StartupOutcomeError::from)?;
    emit_duration(
        MCP_TOOLS_FETCH_UNCACHED_DURATION_METRIC,
        fetch_start.elapsed(),
        &[],
    );
    store_managed_tools(&tool_filter, &tools_arc, tools);

    Ok(ManagedClient {
        session,
        tools: tools_arc,
        _tool_timeout: Some(tool_timeout),
        tool_filter,
    })
}

fn store_managed_tools(
    tool_filter: &ToolFilter,
    tools_arc: &Arc<StdRwLock<Vec<ToolInfo>>>,
    tools: Vec<ToolInfo>,
) -> Vec<ToolInfo> {
    let filtered_tools = filter_tools(tools, tool_filter);
    *tools_arc
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = filtered_tools.clone();
    filtered_tools
}

/// Helper to extract a string from a serde_json object's _meta field.
fn meta_string(meta: Option<&serde_json::Value>, key: &str) -> Option<String> {
    meta.and_then(|meta| meta.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Convert mcp-guest ToolInfo to our internal ToolInfo, extracting connector metadata.
fn guest_tool_to_tool_info(server_name: &str, guest_tool: McpToolInfo) -> ToolInfo {
    let meta = guest_tool.meta.as_ref();
    let connector_id = meta_string(meta, "connector_id");
    let connector_name =
        meta_string(meta, "connector_name").or_else(|| meta_string(meta, "connector_display_name"));
    let connector_description = meta_string(meta, "connector_description")
        .or_else(|| meta_string(meta, "connectorDescription"));

    let tool_name = guest_tool.name.clone();
    let tool_namespace = server_name.to_string();

    ToolInfo {
        server_name: server_name.to_owned(),
        tool_name,
        tool_namespace,
        tool: guest_tool,
        connector_id,
        connector_name,
        connector_description,
    }
}

async fn list_tools_for_session_uncached(
    server_name: &str,
    session: &McpSession,
    timeout: Option<Duration>,
) -> Result<Vec<ToolInfo>> {
    // Bypass the session's built-in cache by issuing the request directly.
    let mut cursor: Option<String> = None;
    let mut all_guest_tools = Vec::new();
    loop {
        let params = mcp_guest::protocol::PaginatedRequestParams {
            cursor: cursor.clone(),
        };
        let result: mcp_guest::protocol::ListToolsResult = session
            .request_with_timeout("tools/list", &params, timeout)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        all_guest_tools.extend(result.tools);
        cursor = result.next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    let tools: Vec<ToolInfo> = all_guest_tools
        .into_iter()
        .map(|tool| guest_tool_to_tool_info(server_name, tool))
        .collect();

    Ok(tools)
}

fn validate_mcp_server_name(server_name: &str) -> Result<()> {
    let re = regex_lite::Regex::new(r"^[a-zA-Z0-9_-]+$")?;
    if !re.is_match(server_name) {
        return Err(anyhow!(
            "Invalid MCP server name '{server_name}': must match pattern {pattern}",
            pattern = re.as_str()
        ));
    }
    Ok(())
}

fn mcp_init_error_display(
    server_name: &str,
    entry: Option<&McpAuthStatusEntry>,
    err: &StartupOutcomeError,
) -> String {
    if let Some(McpServerTransportConfig::StreamableHttp {
        url,
        bearer_token_env_var,
        http_headers,
        ..
    }) = &entry.map(|entry| &entry.config.transport)
        && url == "https://api.githubcopilot.com/mcp/"
        && bearer_token_env_var.is_none()
        && http_headers.as_ref().map(HashMap::is_empty).unwrap_or(true)
    {
        format!(
            "GitHub MCP does not support OAuth. Log in by adding a personal access token (https://github.com/settings/personal-access-tokens) to your environment and config.toml:\n[mcp_servers.{server_name}]\nbearer_token_env_var = CODEX_GITHUB_PERSONAL_ACCESS_TOKEN"
        )
    } else if is_mcp_client_auth_required_error(err) {
        format!(
            "The {server_name} MCP server is not logged in. Run `codex mcp login {server_name}`."
        )
    } else if is_mcp_client_startup_timeout_error(err) {
        let startup_timeout_secs = match entry {
            Some(entry) => match entry.config.startup_timeout_sec {
                Some(timeout) => timeout,
                None => DEFAULT_STARTUP_TIMEOUT,
            },
            None => DEFAULT_STARTUP_TIMEOUT,
        }
        .as_secs();
        format!(
            "MCP client for `{server_name}` timed out after {startup_timeout_secs} seconds. Add or adjust `startup_timeout_sec` in your config.toml:\n[mcp_servers.{server_name}]\nstartup_timeout_sec = XX"
        )
    } else {
        format!("MCP client for `{server_name}` failed to start: {err:#}")
    }
}

fn is_mcp_client_auth_required_error(error: &StartupOutcomeError) -> bool {
    match error {
        StartupOutcomeError::Failed { error } => error.contains("Auth required"),
        _ => false,
    }
}

fn is_mcp_client_startup_timeout_error(error: &StartupOutcomeError) -> bool {
    match error {
        StartupOutcomeError::Failed { error } => {
            error.contains("request timed out")
                || error.contains("timed out handshaking with MCP server")
        }
        _ => false,
    }
}

fn startup_outcome_error_message(error: StartupOutcomeError) -> String {
    match error {
        StartupOutcomeError::Cancelled => "MCP startup cancelled".to_string(),
        StartupOutcomeError::Failed { error } => error,
    }
}

#[cfg(test)]
mod mcp_init_error_display_tests {}

#[cfg(test)]
#[path = "mcp_connection_manager_tests.rs"]
mod tests;
