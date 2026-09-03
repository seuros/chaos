//! Connection manager for Model Context Protocol (MCP) servers.
//!
//! The [`McpConnectionManager`] owns one [`mcp_guest::McpSession`] per
//! configured server (keyed by the *server name*). It offers convenience
//! helpers to query the available tools across *all* servers and returns them
//! in a single aggregated map using the fully-qualified tool name
//! `"<server><MCP_TOOL_NAME_DELIMITER><tool>"` as the key.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use async_channel::Sender;
use chaos_concierge::auth::McpAuthStatusEntry;
use chaos_ipc::mcp::CallToolResult;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::McpStartupCompleteEvent;
use chaos_ipc::protocol::McpStartupFailure;
use chaos_ipc::protocol::McpStartupStatus;
use chaos_ipc::protocol::McpStartupUpdateEvent;
use chaos_sysctl::Constrained;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;
use chaos_traits::McpCatalogSink;
#[cfg(test)]
use futures::FutureExt;
use mcp_guest::ListResourceTemplatesResult;
use mcp_guest::ListResourcesResult;
use mcp_guest::PaginatedRequestParams;
use mcp_guest::ReadResourceRequestParams;
use mcp_guest::ReadResourceResult;
use mcp_guest::ResourceInfo;
use mcp_guest::ResourceTemplateInfo;
pub use mcp_guest::ToolInfo as McpToolInfo;
use mcp_guest::protocol::ElicitationResponse;
use mcp_guest::protocol::RequestId;
use mcp_guest::protocol::TaskOrResult;
use serde::Deserialize;
use serde::Serialize;
use sha1::Digest;
use sha1::Sha1;
use tokio::task::JoinSet;
use tracing::instrument;
use tracing::warn;

#[cfg(test)]
use chaos_sysctl::types::McpServerTransportConfig;

mod client;
mod elicitation;
mod error;
mod filter;
mod handler;

use client::AsyncManagedClient;
use client::ManagedClient;
use elicitation::ElicitationRequestManager;
use error::mcp_init_error_display;
use error::startup_outcome_error_message;
use error::transport_origin;
use filter::StartupOutcomeError;
#[cfg(test)]
use handler::root_uri_from_cwd;

pub use client::CHAOS_MCP_CLIENT_ID_ENV;
pub use client::MCP_SANDBOX_STATE_LOGGER;
pub use client::McpClientIdentity;
pub use client::SandboxState;
pub use filter::ToolFilter;
pub use handler::protocol_request_id_to_guest;

// Items below are only used by the test module via `use super::*`
#[cfg(test)]
use elicitation::elicitation_is_rejected_by_policy;
#[cfg(test)]
use filter::filter_tools;

#[cfg(test)]
fn mcp_client_implementation_version() -> &'static str {
    client::mcp_client_implementation_version()
}

const INITIAL_SUBMIT_ID: &str = "";

/// Reserved `_meta` key for host-attested review provenance.
///
/// Calls through the ordinary MCP tool path may not set this key.
pub const REVIEW_PROVENANCE_META_KEY: &str = "io.chaos.review_provenance.v1";

const ACCOUNT_SUBJECT_PREFIX: &str = "credential:v1:";
const MODEL_FAMILY_SUBJECT_PREFIX: &str = "review-subject:v1:";
const REVIEW_RUN_SUBJECT_PREFIX: &str = "review-run:v1:";
const REVIEWER_ATTEMPT_SUBJECT_PREFIX: &str = "reviewer-attempt:v1:";
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 255;

/// Host-created, wire-safe reviewer provenance.
///
/// Fields are private so callers cannot serialize arbitrary model/provider
/// labels or credentials under the reserved MCP key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrustedReviewProvenance {
    account_subject: String,
    model_family_subject: String,
    review_run_subject: String,
    reviewer_attempt_subject: String,
    idempotency_key: String,
}

impl TrustedReviewProvenance {
    pub fn new(
        account_subject: String,
        model_family_subject: String,
        review_run_subject: String,
        reviewer_attempt_subject: String,
        idempotency_key: String,
    ) -> Result<Self> {
        validate_opaque_subject(&account_subject, ACCOUNT_SUBJECT_PREFIX, "account")?;
        validate_opaque_subject(
            &model_family_subject,
            MODEL_FAMILY_SUBJECT_PREFIX,
            "model family",
        )?;
        validate_opaque_subject(&review_run_subject, REVIEW_RUN_SUBJECT_PREFIX, "review run")?;
        validate_opaque_subject(
            &reviewer_attempt_subject,
            REVIEWER_ATTEMPT_SUBJECT_PREFIX,
            "reviewer attempt",
        )?;
        validate_idempotency_key(&idempotency_key)?;
        Ok(Self {
            account_subject,
            model_family_subject,
            review_run_subject,
            reviewer_attempt_subject,
            idempotency_key,
        })
    }
}

fn validate_opaque_subject(subject: &str, prefix: &str, kind: &str) -> Result<()> {
    let Some(digest) = subject.strip_prefix(prefix) else {
        return Err(anyhow!("{kind} subject is not a recognized opaque subject"));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(anyhow!("{kind} subject has an invalid opaque digest"));
    }
    Ok(())
}

fn validate_idempotency_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY_BYTES {
        return Err(anyhow!(
            "review idempotency key must contain between 1 and {MAX_IDEMPOTENCY_KEY_BYTES} bytes"
        ));
    }
    if !key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(anyhow!(
            "review idempotency key contains non wire-safe characters"
        ));
    }
    Ok(())
}

fn reject_reserved_review_provenance(
    meta: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>> {
    if meta.as_ref().is_some_and(|value| {
        value
            .as_object()
            .is_some_and(|object| object.contains_key(REVIEW_PROVENANCE_META_KEY))
    }) {
        return Err(anyhow!(
            "MCP request metadata key `{REVIEW_PROVENANCE_META_KEY}` is reserved for the host"
        ));
    }
    Ok(meta)
}

fn inject_trusted_review_provenance(
    meta: Option<serde_json::Value>,
    provenance: TrustedReviewProvenance,
) -> Result<Option<serde_json::Value>> {
    let mut object = match meta {
        None => serde_json::Map::new(),
        Some(serde_json::Value::Object(object)) => object,
        Some(_) => {
            return Err(anyhow!(
                "trusted MCP request metadata must be a JSON object"
            ));
        }
    };
    object.insert(
        REVIEW_PROVENANCE_META_KEY.to_string(),
        serde_json::to_value(provenance)
            .context("failed to serialize trusted review provenance")?,
    );
    Ok(Some(serde_json::Value::Object(object)))
}

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
const MCP_SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);

const MIN_COMPATIBLE_MCP_CLIENT_VERSION: &str = "0.63.0";

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
pub struct ToolInfo {
    pub server_name: String,
    pub tool_name: String,
    pub tool_namespace: String,
    pub tool: McpToolInfo,
    pub connector_id: Option<String>,
    pub connector_name: Option<String>,
    pub connector_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerInstructions {
    pub server_name: String,
    pub instructions: String,
}

/// Structured server notifications consumed by the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerNotification {
    ResourceUpdated { server: String, uri: String },
}

async fn emit_update(tx_event: &Sender<Event>, update: McpStartupUpdateEvent) {
    let _ = tx_event
        .send(Event {
            id: INITIAL_SUBMIT_ID.to_owned(),
            msg: EventMsg::McpStartupUpdate(update),
        })
        .await;
}

/// A thin wrapper around a set of running [`mcp_guest::McpSession`] instances.
pub struct McpConnectionManager {
    clients: HashMap<String, AsyncManagedClient>,
    server_origins: HashMap<String, String>,
    elicitation_requests: ElicitationRequestManager,
}

impl McpConnectionManager {
    pub fn new_uninitialized(approval_policy: &Constrained<ApprovalPolicy>) -> Self {
        Self {
            clients: HashMap::new(),
            server_origins: HashMap::new(),
            elicitation_requests: ElicitationRequestManager::new(approval_policy.value()),
        }
    }

    #[cfg(test)]
    pub fn new_mcp_connection_manager_for_tests(
        approval_policy: &Constrained<ApprovalPolicy>,
    ) -> Self {
        Self::new_uninitialized(approval_policy)
    }

    pub fn has_servers(&self) -> bool {
        !self.clients.is_empty()
    }

    pub fn server_origin(&self, server_name: &str) -> Option<&str> {
        self.server_origins.get(server_name).map(String::as_str)
    }

    pub fn set_approval_policy(&self, approval_policy: &Constrained<ApprovalPolicy>) {
        if let Ok(mut policy) = self.elicitation_requests.approval_policy.lock() {
            *policy = approval_policy.value();
        }
    }

    #[allow(clippy::new_ret_no_self, clippy::too_many_arguments)]
    pub async fn new(
        mcp_servers: &HashMap<String, McpServerConfig>,
        client_identities: &HashMap<String, McpClientIdentity>,
        store_mode: OAuthCredentialsStoreMode,
        auth_entries: HashMap<String, McpAuthStatusEntry>,
        approval_policy: &Constrained<ApprovalPolicy>,
        tx_event: Sender<Event>,
        notification_tx: Option<Sender<McpServerNotification>>,
        initial_sandbox_state: SandboxState,
        _codex_home: std::path::PathBuf,
        catalog: Arc<dyn McpCatalogSink>,
    ) -> (Self, tokio_util::sync::CancellationToken) {
        let cancel_token = tokio_util::sync::CancellationToken::new();
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
            let client_identity = client_identities
                .get(&server_name)
                .cloned()
                .unwrap_or_else(|| {
                    warn!(
                        "missing stable client identity for MCP server {server_name}; generating a connection-local fallback"
                    );
                    McpClientIdentity::new()
                });
            emit_update(
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
                client_identity,
                store_mode,
                cancel_token.clone(),
                tx_event.clone(),
                notification_tx.clone(),
                elicitation_requests.clone(),
                Arc::clone(&catalog),
                initial_sandbox_state.clone(),
            );
            clients.insert(server_name.clone(), async_managed_client.clone());
            let tx_event = tx_event.clone();
            let auth_entry = auth_entries.get(&server_name).cloned();
            join_set.spawn(async move {
                let outcome = async_managed_client.client().await;
                if cancel_token.is_cancelled() {
                    return (server_name, Err(StartupOutcomeError::Cancelled));
                }
                let status = match &outcome {
                    Ok(_) => {
                        // Send sandbox state notification immediately after Ready
                        if let Err(e) = async_managed_client.notify_current_sandbox_state().await {
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

                emit_update(
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

    /// Disconnect every initialized server and wait for its transport to finish
    /// shutting down.
    ///
    /// Callers must cancel the generation's startup token before invoking this
    /// method so clients still handshaking resolve as cancelled.
    pub async fn shutdown(&self) -> Result<()> {
        let mut shutdowns = JoinSet::new();
        for (server_name, client) in &self.clients {
            let server_name = server_name.clone();
            let client = client.clone();
            shutdowns.spawn(async move {
                match tokio::time::timeout(MCP_SERVER_SHUTDOWN_TIMEOUT, async {
                    match client.client().await {
                        Ok(client) => match client.session.disconnect().await {
                            Ok(()) | Err(mcp_guest::GuestError::Disconnected) => Ok(()),
                            Err(error) => Err(anyhow!(
                                "failed to disconnect MCP server {server_name}: {error}"
                            )),
                        },
                        Err(
                            StartupOutcomeError::Cancelled | StartupOutcomeError::Failed { .. },
                        ) => Ok(()),
                    }
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(anyhow!(
                        "timed out shutting down MCP server {server_name} after {MCP_SERVER_SHUTDOWN_TIMEOUT:?}"
                    )),
                }
            });
        }

        let mut errors = Vec::new();
        while let Some(result) = shutdowns.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error.to_string()),
                Err(error) => errors.push(format!("MCP shutdown task failed: {error}")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(errors.join("; ")))
        }
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
    pub async fn wait_for_server_ready(&self, server_name: &str, timeout: Duration) -> bool {
        let Some(async_managed_client) = self.clients.get(server_name) else {
            return false;
        };

        match tokio::time::timeout(timeout, async_managed_client.client()).await {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        }
    }

    /// Returns a non-blocking snapshot of every configured client's startup state.
    pub async fn server_startup_statuses(&self) -> HashMap<String, McpStartupStatus> {
        let mut statuses = HashMap::with_capacity(self.clients.len());
        for (server_name, client) in &self.clients {
            let status = if !client
                .startup_complete
                .load(std::sync::atomic::Ordering::Acquire)
            {
                McpStartupStatus::Starting
            } else {
                match client.client().await {
                    Ok(_) => McpStartupStatus::Ready,
                    Err(StartupOutcomeError::Failed { error }) => {
                        McpStartupStatus::Failed { error }
                    }
                    Err(StartupOutcomeError::Cancelled) => McpStartupStatus::Cancelled,
                }
            };
            statuses.insert(server_name.clone(), status);
        }
        statuses
    }

    pub async fn required_startup_failures(
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

    /// Returns non-empty instructions advertised by initialized MCP servers.
    ///
    /// Results are sorted by configured server name so model instructions are
    /// stable across requests despite the manager's `HashMap` storage.
    pub async fn server_instructions(&self) -> Vec<McpServerInstructions> {
        let mut clients: Vec<_> = self
            .clients
            .iter()
            .map(|(server_name, client)| (server_name.clone(), client.clone()))
            .collect();
        clients.sort_by(|(left, _), (right, _)| left.cmp(right));

        let mut instructions = Vec::new();
        for (server_name, async_managed_client) in clients {
            let Ok(managed_client) = async_managed_client.client().await else {
                continue;
            };
            let server_info = managed_client.session.server_info();
            let Some(server_instructions) = server_info.instructions else {
                continue;
            };
            let server_instructions = server_instructions.trim();
            if server_instructions.is_empty() {
                continue;
            }
            instructions.push(McpServerInstructions {
                server_name,
                instructions: server_instructions.to_string(),
            });
        }
        instructions
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
        let meta = reject_reserved_review_provenance(meta)?;
        self.call_tool_inner(server, tool, arguments, meta).await
    }

    /// Invoke a tool with host-attested reviewer provenance.
    ///
    /// Any caller-supplied value under the reserved key is overwritten by the
    /// trusted value after opaque-subject validation.
    pub async fn call_tool_with_review_provenance(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
        provenance: TrustedReviewProvenance,
    ) -> Result<CallToolResult> {
        let meta = inject_trusted_review_provenance(meta, provenance)?;
        self.call_tool_inner(server, tool, arguments, meta).await
    }

    async fn call_tool_inner(
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

        // Reject Required tools on the sync path up front with an actionable
        // error rather than letting the server return a task and then failing late.
        {
            let tools = client
                .tools
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if tools
                .iter()
                .find(|t| t.tool.name == tool)
                .and_then(|t| t.tool.execution.as_ref()?.task_support)
                == Some(mcp_guest::protocol::TaskSupport::Required)
            {
                return Err(anyhow!(
                    "tool '{tool}' on server '{server}' requires async execution \
                     (taskSupport: required); use call_mcp_tool_async instead"
                ));
            }
        }

        // Convert arguments Value to Map<String, Value> for mcp-guest.
        // Non-object shapes are rejected — silently dropping them would turn
        // a caller bug into misleading server behaviour.
        let arguments_map = match arguments {
            None => None,
            Some(serde_json::Value::Object(map)) => Some(map),
            Some(other) => {
                return Err(anyhow!(
                    "tool call arguments must be a JSON object, got {}",
                    other.to_string().chars().take(80).collect::<String>()
                ));
            }
        };

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

        if let Err(err) = client.refresh_listed_tools(server).await {
            warn!(
                "Failed to refresh MCP tools after successful call to '{server}/{tool}': {err:#}"
            );
        }

        let structured_content = result.structured_content;
        let has_structured_content = structured_content
            .as_ref()
            .is_some_and(|value| !value.is_null());
        let content = if has_structured_content {
            Vec::new()
        } else {
            result
                .content
                .into_iter()
                .map(|content| {
                    serde_json::to_value(content)
                        .unwrap_or_else(|_| serde_json::Value::String("<content>".to_string()))
                })
                .collect()
        };

        Ok(CallToolResult {
            content,
            structured_content,
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

    /// Subscribe to updates for a resource on the specified server.
    pub async fn subscribe_resource(&self, server: &str, uri: String) -> Result<()> {
        let managed = self.client_by_name(server).await?;
        ensure_resource_subscriptions_supported(server, &managed.session)?;

        managed
            .session
            .subscribe_resource(uri.as_str())
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("resources/subscribe failed for `{server}` ({uri})"))
    }

    /// Unsubscribe from updates for a resource on the specified server.
    pub async fn unsubscribe_resource(&self, server: &str, uri: String) -> Result<()> {
        let managed = self.client_by_name(server).await?;
        ensure_resource_subscriptions_supported(server, &managed.session)?;

        managed
            .session
            .unsubscribe_resource(uri.as_str())
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("resources/unsubscribe failed for `{server}` ({uri})"))
    }

    /// Invoke a tool with task augmentation, returning the task ID immediately.
    pub async fn call_tool_async(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
        ttl: Option<u64>,
    ) -> Result<mcp_guest::protocol::Task> {
        let meta = reject_reserved_review_provenance(meta)?;
        let client = self.client_by_name(server).await?;
        if !client.tool_filter.allows(tool) {
            return Err(anyhow!(
                "tool '{tool}' is disabled for MCP server '{server}'"
            ));
        }

        // Validate taskSupport before making the wire call. Forbidden means the
        // server will reject task-augmented calls for this tool; Required/Optional
        // are the two valid cases. Absent is treated as Forbidden per spec.
        {
            let tools = client
                .tools
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let tool_info = tools.iter().find(|t| t.tool.name == tool);
            let task_support = tool_info
                .and_then(|t| t.tool.execution.as_ref())
                .and_then(|e| e.task_support);
            match task_support {
                Some(mcp_guest::protocol::TaskSupport::Optional)
                | Some(mcp_guest::protocol::TaskSupport::Required) => {}
                _ => {
                    return Err(anyhow!(
                        "tool '{tool}' on server '{server}' does not support async execution \
                         (taskSupport is Forbidden or absent); use call_mcp_tool instead"
                    ));
                }
            }
        }

        let arguments_map = match arguments {
            None => None,
            Some(serde_json::Value::Object(map)) => Some(map),
            Some(other) => {
                return Err(anyhow!(
                    "async tool call arguments must be a JSON object, got {}",
                    other.to_string().chars().take(80).collect::<String>()
                ));
            }
        };

        let params = mcp_guest::protocol::CallToolRequestParams {
            name: tool.to_string(),
            arguments: arguments_map,
            meta,
            task: Some(mcp_guest::protocol::TaskMetadata { ttl }),
        };

        let response = client
            .session
            .call_tool_with(params)
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("async tool call failed for `{server}/{tool}`"))?;

        match response {
            TaskOrResult::Task(task_result) => Ok(task_result.task),
            TaskOrResult::Result(_) => Err(anyhow!(
                "server '{server}' returned a direct result for task-augmented call to '{tool}'; \
                 server must declare tasks capability"
            )),
        }
    }

    /// Poll task status.
    pub async fn get_task(&self, server: &str, task_id: &str) -> Result<mcp_guest::protocol::Task> {
        let client = self.client_by_name(server).await?;
        client
            .session
            .get_task(task_id)
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("tasks/get failed for `{server}` ({task_id})"))
    }

    /// Block until a task reaches a terminal state and return its result.
    pub async fn get_task_result(
        &self,
        server: &str,
        task_id: &str,
    ) -> Result<mcp_guest::protocol::CallToolResult> {
        let client = self.client_by_name(server).await?;
        let raw: serde_json::Value = client
            .session
            .request_value(
                "tasks/result",
                Some(serde_json::json!({ "taskId": task_id })),
            )
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("tasks/result failed for `{server}` ({task_id})"))?;
        serde_json::from_value(raw).with_context(|| {
            format!("failed to deserialize tasks/result for `{server}` ({task_id})")
        })
    }

    /// List tasks from a server (paginates all pages).
    pub async fn list_tasks(&self, server: &str) -> Result<mcp_guest::ListTasksResult> {
        let client = self.client_by_name(server).await?;
        client
            .session
            .list_tasks()
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("tasks/list failed for `{server}`"))
    }

    /// Cancel a running task.
    pub async fn cancel_task(
        &self,
        server: &str,
        task_id: &str,
    ) -> Result<mcp_guest::protocol::Task> {
        let client = self.client_by_name(server).await?;
        client
            .session
            .cancel_task(task_id)
            .await
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("tasks/cancel failed for `{server}` ({task_id})"))
    }

    pub async fn parse_tool_name(&self, tool_name: &str) -> Option<(String, String)> {
        self.list_all_tools()
            .await
            .get(tool_name)
            .map(|tool| (tool.server_name.clone(), tool.tool.name.to_string()))
    }

    /// Notifies all MCP servers that the workspace root has changed.
    pub async fn notify_roots_changed(&self, new_cwd: &Path) -> Result<()> {
        let mut join_set = JoinSet::new();

        for async_managed_client in self.clients.values() {
            let new_cwd = new_cwd.to_path_buf();
            let async_managed_client = async_managed_client.clone();
            join_set
                .spawn(async move { async_managed_client.notify_roots_changed(&new_cwd).await });
        }

        while let Some(join_res) = join_set.join_next().await {
            match join_res {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    warn!("Failed to notify roots change to MCP server: {err:#}");
                }
                Err(err) => {
                    warn!("Task panic when notifying roots change to MCP server: {err:#}");
                }
            }
        }

        Ok(())
    }

    /// Notifies one MCP server that the workspace root has changed.
    pub async fn notify_server_roots_changed(&self, server: &str, new_cwd: &Path) -> Result<()> {
        let managed = self
            .clients
            .get(server)
            .ok_or_else(|| anyhow!("unknown MCP server '{server}'"))?;
        managed.notify_roots_changed(new_cwd).await
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

    /// Sends the current sandbox state to one MCP server.
    pub async fn notify_server_sandbox_state_change(
        &self,
        server: &str,
        sandbox_state: &SandboxState,
    ) -> Result<()> {
        let managed = self
            .clients
            .get(server)
            .ok_or_else(|| anyhow!("unknown MCP server '{server}'"))?;
        managed.notify_sandbox_state_change(sandbox_state).await
    }
}

fn ensure_resource_subscriptions_supported(
    server: &str,
    session: &mcp_guest::McpSession,
) -> Result<()> {
    let server_info = session.server_info();
    if resource_subscriptions_supported(&server_info.capabilities) {
        Ok(())
    } else {
        Err(anyhow!(
            "MCP server `{server}` does not advertise resource subscription support"
        ))
    }
}

fn resource_subscriptions_supported(
    capabilities: &mcp_guest::protocol::ServerCapabilities,
) -> bool {
    capabilities
        .resources
        .as_ref()
        .and_then(|resources| resources.subscribe)
        .unwrap_or(false)
}

#[cfg(test)]
mod mcp_init_error_display_tests {}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
