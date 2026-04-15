use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;

use async_channel::Sender;
use chaos_ipc::approvals::ElicitationCompleteEvent;
use chaos_ipc::approvals::ElicitationRequest;
use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::mcp::RequestId as ProtocolRequestId;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_traits::McpCatalogSink;
use mcp_guest::ClientHandler;
use mcp_guest::ClientHandlerFuture;
use mcp_guest::ClientHandlerResultFuture;
use mcp_guest::McpSession;
use mcp_guest::protocol::CreateElicitationRequest;
use mcp_guest::protocol::CreateElicitationResponse;
use mcp_guest::protocol::ElicitationAction;
use mcp_guest::protocol::ElicitationCompleteNotificationParams;
use mcp_guest::protocol::ListRootsResult;
use mcp_guest::protocol::RequestId;
use mcp_guest::protocol::Root;
use mcp_guest::protocol::TaskOrResult;
use tokio::sync::oneshot;
use tracing::warn;

use super::ToolInfo;
use super::elicitation::ElicitationRequestManager;
use super::elicitation::elicitation_is_rejected_by_policy;
use super::filter::ToolFilter;
use super::filter::store_managed_tools;

pub(super) fn request_id_to_protocol(id: &RequestId) -> ProtocolRequestId {
    match id {
        RequestId::String(s) => ProtocolRequestId::String(s.clone()),
        RequestId::Number(n) => ProtocolRequestId::Integer(*n),
    }
}

/// Convert protocol RequestId to mcp-guest RequestId.
pub fn protocol_request_id_to_guest(id: &ProtocolRequestId) -> RequestId {
    match id {
        ProtocolRequestId::String(s) => RequestId::string(s.clone()),
        ProtocolRequestId::Integer(n) => RequestId::number(*n),
    }
}

pub(super) fn root_uri_from_cwd(cwd: &std::path::Path) -> String {
    use tracing::warn;
    use url::Url;
    Url::from_directory_path(cwd)
        .or_else(|()| Url::from_file_path(cwd))
        .map(|url| url.to_string())
        .unwrap_or_else(|()| {
            warn!("Failed to convert cwd to file URI: {}", cwd.display());
            "file:///".to_string()
        })
}

/// Handler that bridges mcp-guest callbacks to the core event system.
pub(super) struct ChaosClientHandler {
    pub(super) server_name: String,
    pub(super) tx_event: Sender<Event>,
    pub(super) elicitation_requests: ElicitationRequestManager,
    /// Shared tool store + filter for refreshing on list_changed.
    pub(super) tools_arc: Arc<StdRwLock<Vec<ToolInfo>>>,
    pub(super) tool_filter: ToolFilter,
    pub(super) tool_timeout: Duration,
    /// The session is set after connect. We need it for re-listing tools
    /// when the server sends a tools/list_changed notification.
    pub(super) session: Arc<tokio::sync::RwLock<Option<McpSession>>>,
    /// Shared catalog for updating on list_changed notifications.
    pub(super) catalog: Arc<dyn McpCatalogSink>,
    /// Working directory exposed to MCP servers via roots/list.
    pub(super) cwd: Arc<StdRwLock<PathBuf>>,
}

impl ClientHandler for ChaosClientHandler {
    fn list_roots(&self) -> ClientHandlerResultFuture<'_, ListRootsResult> {
        let cwd = self
            .cwd
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move {
            Ok(ListRootsResult {
                roots: vec![Root {
                    uri: root_uri_from_cwd(&cwd),
                    name: None,
                }],
            })
        })
    }

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
            refresh_tools(
                &self.server_name,
                session,
                self.tool_timeout,
                &self.tool_filter,
                &self.tools_arc,
                &*self.catalog,
            )
            .await;
        })
    }

    fn on_resources_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async move {
            let session_guard = self.session.read().await;
            let Some(session) = session_guard.as_ref() else {
                return;
            };
            refresh_resources(&self.server_name, session, &*self.catalog).await;
        })
    }

    fn on_prompts_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async move {
            let session_guard = self.session.read().await;
            let Some(session) = session_guard.as_ref() else {
                return;
            };
            refresh_prompts(&self.server_name, session, &*self.catalog).await;
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

/// Refresh tools for `server_name`: re-list from the session, apply the filter
/// into `tools_arc`, then sync the filtered set to the catalog.
async fn refresh_tools(
    server_name: &str,
    session: &McpSession,
    tool_timeout: Duration,
    tool_filter: &ToolFilter,
    tools_arc: &Arc<StdRwLock<Vec<ToolInfo>>>,
    catalog: &dyn McpCatalogSink,
) {
    match super::client::list_tools_for_session_uncached(server_name, session, Some(tool_timeout))
        .await
    {
        Ok(tools) => {
            store_managed_tools(tool_filter, tools_arc, tools);
            if let Ok(store) = tools_arc.read() {
                let catalog_tools: Vec<_> = store
                    .iter()
                    .map(crate::catalog_conv::mcp_tool_info_to_catalog_tool)
                    .collect();
                catalog.unregister_mcp(server_name);
                catalog.register_mcp_tools(server_name, catalog_tools);
            }
        }
        Err(err) => {
            warn!("Failed to refresh tool list for '{server_name}': {err}");
        }
    }
}

/// Re-list resources and resource templates from `session` and push them to
/// the catalog, replacing whatever was registered before.
async fn refresh_resources(server_name: &str, session: &McpSession, catalog: &dyn McpCatalogSink) {
    let resources = match session.list_resources().await {
        Ok(list) => list
            .iter()
            .map(crate::catalog_conv::mcp_resource_to_catalog)
            .collect(),
        Err(err) => {
            warn!("Failed to refresh resource list for '{server_name}': {err}");
            return;
        }
    };

    let templates = match session.list_resource_templates().await {
        Ok(list) => list
            .iter()
            .map(crate::catalog_conv::mcp_resource_template_to_catalog)
            .collect(),
        Err(err) => {
            warn!("Failed to refresh resource template list for '{server_name}': {err}");
            Vec::new()
        }
    };

    // `unregister_mcp` would also drop tools and prompts, so we use the
    // narrower resource-only unregister here.
    catalog.unregister_mcp_resources(server_name);
    catalog.register_mcp_resources(server_name, resources, templates);
}

/// Re-list prompts from `session` and push them to the catalog.
async fn refresh_prompts(server_name: &str, session: &McpSession, catalog: &dyn McpCatalogSink) {
    let prompts = match session.list_prompts().await {
        Ok(result) => result
            .iter()
            .map(crate::catalog_conv::mcp_prompt_to_catalog)
            .collect(),
        Err(err) => {
            warn!("Failed to refresh prompt list for '{server_name}': {err}");
            return;
        }
    };

    catalog.unregister_mcp_prompts(server_name);
    catalog.register_mcp_prompts(server_name, prompts);
}
