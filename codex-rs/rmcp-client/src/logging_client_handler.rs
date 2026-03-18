use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use rmcp::ClientHandler;
use rmcp::RoleClient;
use rmcp::model::CancelledNotificationParam;
use rmcp::model::ClientInfo;
use rmcp::model::CreateElicitationRequestParams;
use rmcp::model::CreateElicitationResult;
use rmcp::model::LoggingLevel;
use rmcp::model::LoggingMessageNotificationParam;
use rmcp::model::ProgressNotificationParam;
use rmcp::model::RequestId;
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::service::NotificationContext;
use rmcp::service::RequestContext;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::rmcp_client::OnToolListChanged;
use crate::rmcp_client::OnUrlElicitationComplete;
use crate::rmcp_client::SendElicitation;

#[derive(Clone)]
pub(crate) struct LoggingClientHandler {
    client_info: ClientInfo,
    send_elicitation: Arc<SendElicitation>,
    next_synthetic_request_id: Arc<AtomicU64>,
    on_url_elicitation_complete: Arc<OnUrlElicitationComplete>,
    on_tool_list_changed: Arc<OnToolListChanged>,
}

impl LoggingClientHandler {
    pub(crate) fn new(
        client_info: ClientInfo,
        send_elicitation: SendElicitation,
        on_url_elicitation_complete: OnUrlElicitationComplete,
        on_tool_list_changed: OnToolListChanged,
    ) -> Self {
        Self {
            client_info,
            send_elicitation: Arc::new(send_elicitation),
            next_synthetic_request_id: Arc::new(AtomicU64::new(0)),
            on_url_elicitation_complete: Arc::new(on_url_elicitation_complete),
            on_tool_list_changed: Arc::new(on_tool_list_changed),
        }
    }

    pub(crate) async fn dispatch_url_elicitation_required(
        &self,
        requests: Vec<CreateElicitationRequestParams>,
    ) {
        for request in requests {
            let synthetic_id = self
                .next_synthetic_request_id
                .fetch_add(1, Ordering::Relaxed);
            let request_id =
                RequestId::String(Arc::<str>::from(format!("url-required-{synthetic_id}")));
            match (self.send_elicitation)(request_id, request).await {
                Ok(result) => {
                    info!(
                        "forwarded URL-required elicitation completed with action: {:?}",
                        result.action
                    );
                }
                Err(err) => {
                    warn!("failed to forward URL-required elicitation: {err}");
                }
            }
        }
    }
}

impl ClientHandler for LoggingClientHandler {
    async fn create_elicitation(
        &self,
        request: CreateElicitationRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<CreateElicitationResult, rmcp::ErrorData> {
        (self.send_elicitation)(context.id, request)
            .await
            .map(Into::into)
            .map_err(|err| rmcp::ErrorData::internal_error(err.to_string(), None))
    }

    async fn on_cancelled(
        &self,
        params: CancelledNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        info!(
            "MCP server cancelled request (request_id: {}, reason: {:?})",
            params.request_id, params.reason
        );
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        info!(
            "MCP server progress notification (token: {:?}, progress: {}, total: {:?}, message: {:?})",
            params.progress_token, params.progress, params.total, params.message
        );
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        info!("MCP server resource updated (uri: {})", params.uri);
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        info!("MCP server resource list changed");
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        info!("MCP server tool list changed");
        (self.on_tool_list_changed)().await;
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        info!("MCP server prompt list changed");
    }

    async fn on_url_elicitation_notification_complete(
        &self,
        params: rmcp::model::ElicitationResponseNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        info!(
            "MCP server URL elicitation completed (elicitation_id: {})",
            params.elicitation_id
        );
        (self.on_url_elicitation_complete)(params.elicitation_id).await;
    }

    fn get_info(&self) -> ClientInfo {
        self.client_info.clone()
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let LoggingMessageNotificationParam {
            level,
            logger,
            data,
        } = params;
        let logger = logger.as_deref();
        match level {
            LoggingLevel::Emergency
            | LoggingLevel::Alert
            | LoggingLevel::Critical
            | LoggingLevel::Error => {
                error!(
                    "MCP server log message (level: {:?}, logger: {:?}, data: {})",
                    level, logger, data
                );
            }
            LoggingLevel::Warning => {
                warn!(
                    "MCP server log message (level: {:?}, logger: {:?}, data: {})",
                    level, logger, data
                );
            }
            LoggingLevel::Notice | LoggingLevel::Info => {
                info!(
                    "MCP server log message (level: {:?}, logger: {:?}, data: {})",
                    level, logger, data
                );
            }
            LoggingLevel::Debug => {
                debug!(
                    "MCP server log message (level: {:?}, logger: {:?}, data: {})",
                    level, logger, data
                );
            }
        }
    }
}
