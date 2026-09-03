use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use super::McpServerNotification;
use super::ToolInfo;
use super::elicitation::ElicitationRequestManager;
use super::error::MakeClientParams;
use super::filter::StartupOutcomeError;
use super::filter::ToolFilter;
use super::filter::filter_tools;
use super::filter::store_managed_tools;
use super::handler::ChaosClientHandler;
use anyhow::Result;
use anyhow::anyhow;
use chaos_epoll::CancelErr;
use chaos_epoll::OrCancelExt;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::product::OS_NAME;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::McpServerTransportConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;
use futures::future::BoxFuture;
use futures::future::FutureExt;
use futures::future::Shared;
use mcp_guest::McpSession;
use mcp_guest::protocol::PaginatedRequestParams;
use serde::Deserialize;
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) const MCP_TOOLS_FETCH_UNCACHED_DURATION_METRIC: &str =
    "chaos.mcp.tools.fetch_uncached.duration_ms";
const MCP_CLIENT_ID_DESCRIPTION_PREFIX: &str = "chaos-mcp-client-id:";
/// Trusted identity injected into every managed stdio MCP child.
pub const CHAOS_MCP_CLIENT_ID_ENV: &str = "CHAOS_MCP_CLIENT_ID";

/// Logger name used to identify sandbox state notifications.
pub const MCP_SANDBOX_STATE_LOGGER: &str = "chaos/alcatraz-state";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxState {
    pub vfs_policy: VfsPolicy,
    pub socket_policy: SocketPolicy,
    pub alcatraz_macos_exe: Option<PathBuf>,
    pub alcatraz_linux_exe: Option<PathBuf>,
    pub alcatraz_freebsd_exe: Option<PathBuf>,
    pub sandbox_cwd: PathBuf,
}

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

fn create_env_for_mcp_server(
    extra_env: Option<HashMap<String, String>>,
    env_vars: &[String],
    client_identity: &McpClientIdentity,
) -> HashMap<String, String> {
    let mut env = DEFAULT_ENV_VARS
        .iter()
        .copied()
        .chain(env_vars.iter().map(String::as_str))
        .filter_map(|var| env::var(var).ok().map(|value| (var.to_string(), value)))
        .chain(extra_env.unwrap_or_default())
        .collect::<HashMap<_, _>>();
    env.insert(
        CHAOS_MCP_CLIENT_ID_ENV.to_string(),
        client_identity.trusted_stdio_subject(),
    );
    env
}

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

fn resolve_bearer_token(
    server_name: &str,
    bearer_token: Option<&str>,
    bearer_token_env_var: Option<&str>,
) -> Result<Option<String>> {
    if let Some(token) = bearer_token {
        if token.is_empty() {
            return Err(anyhow!(
                "Configured bearer token for MCP server '{server_name}' is empty"
            ));
        }
        return Ok(Some(token.to_string()));
    }

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

fn emit_duration(metric: &str, duration: Duration, tags: &[(&str, &str)]) {
    if let Some(metrics) = chaos_snitch::metrics::global() {
        let _ = metrics.record_duration(metric, duration, tags);
    }
}

pub(super) fn validate_mcp_server_name(server_name: &str) -> Result<()> {
    let re = regex_lite::Regex::new(r"^[a-zA-Z0-9_-]+$")?;
    if !re.is_match(server_name) {
        return Err(anyhow!(
            "Invalid MCP server name '{server_name}': must match pattern {pattern}",
            pattern = re.as_str()
        ));
    }
    Ok(())
}

pub(super) fn mcp_client_implementation_version() -> &'static str {
    use chaos_ipc::product::CHAOS_VERSION;
    let version = CHAOS_VERSION;
    if version == "0.0.0" {
        super::MIN_COMPATIBLE_MCP_CLIENT_VERSION
    } else {
        version
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpClientIdentity(String);

impl McpClientIdentity {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Derive a stable identity for one host-controlled session and MCP server.
    ///
    /// The scope must come from trusted host state, never model or MCP input.
    pub fn for_scope(scope: &str, server_name: &str) -> Self {
        let material = format!("chaos-mcp-client-id:v1\0{scope}\0{server_name}");
        Self(Uuid::new_v5(&Uuid::NAMESPACE_OID, material.as_bytes()).to_string())
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn trusted_stdio_subject(&self) -> String {
        format!("chaos:{}", self.as_str())
    }
}

impl Default for McpClientIdentity {
    fn default() -> Self {
        Self::new()
    }
}

fn managed_client_info(identity: &McpClientIdentity) -> mcp_guest::protocol::Implementation {
    mcp_guest::protocol::Implementation::new(
        "chaos-mcp-client",
        mcp_client_implementation_version(),
    )
    .with_title(OS_NAME)
    .with_description(format!(
        "{MCP_CLIENT_ID_DESCRIPTION_PREFIX}{}",
        identity.as_str()
    ))
}

pub(super) async fn list_tools_for_session_uncached(
    server_name: &str,
    session: &McpSession,
    timeout: Option<Duration>,
) -> Result<Vec<ToolInfo>> {
    use mcp_guest::protocol::ListToolsResult;

    // Bypass the session's built-in cache by issuing the request directly.
    let mut cursor: Option<String> = None;
    let mut all_guest_tools = Vec::new();
    loop {
        let params = PaginatedRequestParams {
            cursor: cursor.clone(),
        };
        let result: ListToolsResult = session
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

fn meta_string(meta: Option<&serde_json::Value>, key: &str) -> Option<String> {
    meta.and_then(|meta| meta.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn guest_tool_to_tool_info(server_name: &str, guest_tool: super::McpToolInfo) -> ToolInfo {
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

#[derive(Clone)]
pub(super) struct ManagedClient {
    pub(super) session: McpSession,
    pub(super) tools: Arc<StdRwLock<Vec<ToolInfo>>>,
    pub(super) tool_filter: ToolFilter,
    pub(super) tool_timeout: Option<Duration>,
    /// Shared cwd for roots/list — updated when the workspace root changes.
    pub(super) cwd: Arc<StdRwLock<PathBuf>>,
}

impl ManagedClient {
    pub(super) fn listed_tools(&self) -> Vec<ToolInfo> {
        let in_memory_tools = self
            .tools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        filter_tools(in_memory_tools, &self.tool_filter)
    }

    /// Re-list tools without using the guest cache and update the store read by
    /// the next model sampling request.
    pub(super) async fn refresh_listed_tools(&self, server_name: &str) -> Result<()> {
        let fetch_start = Instant::now();
        let tools =
            list_tools_for_session_uncached(server_name, &self.session, self.tool_timeout).await?;
        emit_duration(
            MCP_TOOLS_FETCH_UNCACHED_DURATION_METRIC,
            fetch_start.elapsed(),
            &[("server", server_name)],
        );
        store_managed_tools(&self.tool_filter, &self.tools, tools);
        Ok(())
    }

    /// Updates the shared cwd and sends `notifications/roots/list_changed` to the server.
    pub(super) async fn notify_roots_changed(&self, new_cwd: &Path) -> Result<()> {
        {
            let mut cwd = self
                .cwd
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *cwd = new_cwd.to_path_buf();
        }
        self.session
            .notify_value("notifications/roots/list_changed", None)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    /// Sends sandbox state as a standard MCP log notification.
    pub(super) async fn notify_sandbox_state_change(
        &self,
        sandbox_state: &SandboxState,
    ) -> Result<()> {
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
pub(in crate::manager) struct AsyncManagedClient {
    pub(in crate::manager) client:
        Shared<BoxFuture<'static, Result<ManagedClient, StartupOutcomeError>>>,
    pub(in crate::manager) startup_snapshot: Option<Vec<ToolInfo>>,
    pub(in crate::manager) startup_complete: Arc<AtomicBool>,
    pub(in crate::manager) cwd: Arc<StdRwLock<PathBuf>>,
    sandbox_state: Arc<StdRwLock<SandboxState>>,
    sandbox_notification_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AsyncManagedClient {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        server_name: String,
        config: McpServerConfig,
        client_identity: McpClientIdentity,
        _store_mode: OAuthCredentialsStoreMode,
        cancel_token: CancellationToken,
        tx_event: async_channel::Sender<chaos_ipc::protocol::Event>,
        notification_tx: Option<async_channel::Sender<McpServerNotification>>,
        elicitation_requests: ElicitationRequestManager,
        catalog: Arc<dyn chaos_traits::McpCatalogSink>,
        initial_sandbox_state: SandboxState,
    ) -> Self {
        let tool_filter = ToolFilter::from_config(&config);
        let startup_tool_filter = tool_filter;
        let startup_complete = Arc::new(AtomicBool::new(false));
        let startup_complete_for_fut = Arc::clone(&startup_complete);
        let cwd = Arc::new(StdRwLock::new(initial_sandbox_state.sandbox_cwd.clone()));
        let cwd_for_client = Arc::clone(&cwd);
        let sandbox_state = Arc::new(StdRwLock::new(initial_sandbox_state));
        let fut = async move {
            let outcome = async {
                if let Err(error) = validate_mcp_server_name(&server_name) {
                    return Err(error.into());
                }

                make_managed_client(
                    server_name,
                    config,
                    client_identity,
                    MakeClientParams {
                        tool_filter: startup_tool_filter,
                        tx_event,
                        notification_tx,
                        elicitation_requests,
                        catalog,
                        cwd: cwd_for_client,
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
            cwd,
            sandbox_state,
            sandbox_notification_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(super) async fn client(&self) -> Result<ManagedClient, StartupOutcomeError> {
        self.client.clone().await
    }

    #[cfg(test)]
    pub(in crate::manager) fn for_tests(
        client: Shared<BoxFuture<'static, Result<ManagedClient, StartupOutcomeError>>>,
        startup_snapshot: Option<Vec<ToolInfo>>,
        startup_complete: Arc<AtomicBool>,
        cwd: Arc<StdRwLock<PathBuf>>,
    ) -> Self {
        let sandbox_cwd = cwd.read().expect("MCP root lock poisoned").clone();
        Self {
            client,
            startup_snapshot,
            startup_complete,
            cwd,
            sandbox_state: Arc::new(StdRwLock::new(SandboxState {
                vfs_policy: VfsPolicy::default(),
                socket_policy: SocketPolicy::default(),
                alcatraz_macos_exe: None,
                alcatraz_linux_exe: None,
                alcatraz_freebsd_exe: None,
                sandbox_cwd,
            })),
            sandbox_notification_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn startup_snapshot_while_initializing(&self) -> Option<Vec<ToolInfo>> {
        if !self.startup_complete.load(Ordering::Acquire) {
            return self.startup_snapshot.clone();
        }
        None
    }

    pub(super) async fn listed_tools(&self) -> Option<Vec<ToolInfo>> {
        if let Some(startup_tools) = self.startup_snapshot_while_initializing() {
            Some(startup_tools)
        } else {
            match self.client().await {
                Ok(client) => Some(client.listed_tools()),
                Err(_) => self.startup_snapshot.clone(),
            }
        }
    }

    pub(super) async fn notify_roots_changed(&self, new_cwd: &Path) -> Result<()> {
        {
            let mut cwd = self
                .cwd
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *cwd = new_cwd.to_path_buf();
        }
        if !self.startup_complete.load(Ordering::Acquire) {
            return Ok(());
        }
        let managed = self.client().await?;
        managed.notify_roots_changed(new_cwd).await
    }

    pub(super) async fn notify_sandbox_state_change(
        &self,
        sandbox_state: &SandboxState,
    ) -> Result<()> {
        {
            let mut current = self
                .sandbox_state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *current = sandbox_state.clone();
        }
        if !self.startup_complete.load(Ordering::Acquire) {
            return Ok(());
        }
        let Ok(managed) = self.client().await else {
            return Ok(());
        };
        self.notify_sandbox_state(&managed).await
    }

    pub(super) async fn notify_current_sandbox_state(&self) -> Result<()> {
        let managed = self.client().await?;
        self.notify_sandbox_state(&managed).await
    }

    async fn notify_sandbox_state(&self, managed: &ManagedClient) -> Result<()> {
        let _notification_guard = self.sandbox_notification_lock.lock().await;
        let sandbox_state = self
            .sandbox_state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        managed.notify_sandbox_state_change(&sandbox_state).await
    }
}

pub(super) async fn make_managed_client(
    server_name: String,
    config: McpServerConfig,
    client_identity: McpClientIdentity,
    params: MakeClientParams,
) -> Result<ManagedClient, StartupOutcomeError> {
    let MakeClientParams {
        tool_filter,
        tx_event,
        notification_tx,
        elicitation_requests,
        catalog,
        cwd,
    } = params;

    let tool_timeout = config
        .tool_timeout_sec
        .unwrap_or(super::DEFAULT_TOOL_TIMEOUT);
    let startup_timeout = config
        .startup_timeout_sec
        .or(Some(super::DEFAULT_STARTUP_TIMEOUT));

    let tools_arc: Arc<StdRwLock<Vec<ToolInfo>>> = Arc::new(StdRwLock::new(Vec::new()));
    let session_holder: Arc<tokio::sync::RwLock<Option<mcp_guest::WeakMcpSession>>> =
        Arc::new(tokio::sync::RwLock::new(None));
    let cwd_arc = cwd;

    let handler = ChaosClientHandler {
        server_name: server_name.clone(),
        tx_event,
        notification_tx,
        elicitation_requests,
        tools_arc: Arc::clone(&tools_arc),
        tool_filter: tool_filter.clone(),
        tool_timeout,
        session: Arc::clone(&session_holder),
        catalog,
        cwd: Arc::clone(&cwd_arc),
    };

    let client_info = managed_client_info(&client_identity);

    let capabilities = mcp_guest::protocol::ClientCapabilities {
        experimental: None,
        roots: Some(mcp_guest::protocol::RootsCapability {
            list_changed: Some(true),
        }),
        sampling: Some(mcp_guest::protocol::SamplingCapability {
            context: None,
            tools: None,
        }),
        elicitation: Some(mcp_guest::protocol::ElicitationCapability {
            form: Some(mcp_guest::protocol::FormElicitationCapability {}),
            url: Some(mcp_guest::protocol::UrlElicitationCapability {}),
        }),
        tasks: Some(mcp_guest::protocol::TasksCapability {
            list: Some(mcp_guest::protocol::EmptyObject {}),
            cancel: Some(mcp_guest::protocol::EmptyObject {}),
            requests: Some(mcp_guest::protocol::TasksRequestsCapability {
                tools: Some(mcp_guest::protocol::TasksToolsCapability {
                    call: Some(mcp_guest::protocol::EmptyObject {}),
                }),
                sampling: None,
                elicitation: None,
            }),
        }),
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
                let envs = create_env_for_mcp_server(env, &env_vars, &client_identity);
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
                bearer_token,
                http_headers,
                env_http_headers,
                bearer_token_env_var,
            } => {
                let resolved_bearer_token = resolve_bearer_token(
                    &server_name,
                    bearer_token.as_deref(),
                    bearer_token_env_var.as_deref(),
                )?;

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
    *session_holder.write().await = Some(session.downgrade());

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
        tool_timeout: Some(tool_timeout),
        tool_filter,
        cwd: cwd_arc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future;

    #[test]
    fn managed_client_info_reuses_the_supplied_identity() {
        let identity = McpClientIdentity::new();
        let first = managed_client_info(&identity);
        let second = managed_client_info(&identity);
        let first_id = first
            .description
            .as_deref()
            .and_then(|description| description.strip_prefix(MCP_CLIENT_ID_DESCRIPTION_PREFIX))
            .expect("managed client identity");
        let second_id = second
            .description
            .as_deref()
            .and_then(|description| description.strip_prefix(MCP_CLIENT_ID_DESCRIPTION_PREFIX))
            .expect("managed client identity");

        assert!(Uuid::parse_str(first_id).is_ok());
        assert_eq!(first_id, second_id);
    }

    #[test]
    fn managed_client_identities_are_unique_when_created() {
        assert_ne!(McpClientIdentity::new(), McpClientIdentity::new());
    }

    #[test]
    fn scoped_client_identities_are_stable_and_isolated() {
        let first = McpClientIdentity::for_scope("session-1", "review-service");
        let replay = McpClientIdentity::for_scope("session-1", "review-service");
        let other_server = McpClientIdentity::for_scope("session-1", "other");
        let other_session = McpClientIdentity::for_scope("session-2", "review-service");

        assert_eq!(first, replay);
        assert_ne!(first, other_server);
        assert_ne!(first, other_session);
    }

    #[test]
    fn stdio_environment_uses_the_stable_client_identity_and_rejects_spoofing() {
        let identity = McpClientIdentity::new();
        let configured = HashMap::from([(
            CHAOS_MCP_CLIENT_ID_ENV.to_string(),
            "chaos:spoofed".to_string(),
        )]);

        let env = create_env_for_mcp_server(Some(configured), &[], &identity);

        assert_eq!(
            env.get(CHAOS_MCP_CLIENT_ID_ENV),
            Some(&identity.trusted_stdio_subject())
        );
    }

    fn sandbox_state(cwd: &str) -> SandboxState {
        SandboxState {
            vfs_policy: VfsPolicy::default(),
            socket_policy: SocketPolicy::default(),
            alcatraz_macos_exe: None,
            alcatraz_linux_exe: None,
            alcatraz_freebsd_exe: None,
            sandbox_cwd: PathBuf::from(cwd),
        }
    }

    fn async_client(
        initial: SandboxState,
        startup_complete: bool,
        client: Result<ManagedClient, StartupOutcomeError>,
    ) -> AsyncManagedClient {
        AsyncManagedClient::for_tests(
            future::ready(client).boxed().shared(),
            None,
            Arc::new(AtomicBool::new(startup_complete)),
            Arc::new(StdRwLock::new(initial.sandbox_cwd)),
        )
    }

    #[tokio::test]
    async fn sandbox_update_is_retained_while_server_is_starting() {
        let initial = sandbox_state("/initial");
        let client = async_client(initial, false, Err(StartupOutcomeError::Cancelled));
        let updated = sandbox_state("/updated");

        client
            .notify_sandbox_state_change(&updated)
            .await
            .expect("queue sandbox state");

        assert_eq!(
            client
                .sandbox_state
                .read()
                .expect("MCP sandbox state lock poisoned")
                .sandbox_cwd,
            PathBuf::from("/updated")
        );
    }

    #[tokio::test]
    async fn sandbox_update_ignores_already_reported_startup_failure() {
        let initial = sandbox_state("/initial");
        let client = async_client(initial, true, Err(StartupOutcomeError::Cancelled));

        client
            .notify_sandbox_state_change(&sandbox_state("/updated"))
            .await
            .expect("failed optional server should not reject permission updates");
    }
}
