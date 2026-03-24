use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::error::GuestError;
use crate::protocol::{
    CreateElicitationRequest, CreateElicitationResponse, CreateMessageRequest,
    CreateMessageResponse, ElicitationCompleteNotificationParams, ListRootsResult,
    LogMessageNotificationParams, ProgressNotificationParams, ResourceUpdatedNotificationParams,
    Task,
};

pub type ClientHandlerFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
pub type ClientHandlerResultFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, GuestError>> + Send + 'a>>;

pub trait ClientHandler: Send + Sync + 'static {
    fn handle_ping(&self) -> ClientHandlerResultFuture<'_, Value> {
        Box::pin(async { Ok(serde_json::json!({})) })
    }

    fn list_roots(&self) -> ClientHandlerResultFuture<'_, ListRootsResult> {
        Box::pin(async { Err(GuestError::MethodNotSupported("roots/list".to_string())) })
    }

    fn create_message(
        &self,
        _request: CreateMessageRequest,
    ) -> ClientHandlerResultFuture<'_, CreateMessageResponse> {
        Box::pin(async {
            Err(GuestError::MethodNotSupported(
                "sampling/createMessage".to_string(),
            ))
        })
    }

    fn create_elicitation(
        &self,
        _request: CreateElicitationRequest,
    ) -> ClientHandlerResultFuture<'_, CreateElicitationResponse> {
        Box::pin(async {
            Err(GuestError::MethodNotSupported(
                "elicitation/create".to_string(),
            ))
        })
    }

    fn on_log_message(&self, _params: LogMessageNotificationParams) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_progress(&self, _params: ProgressNotificationParams) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_tools_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_resources_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_prompts_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_roots_list_changed(&self) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_resource_updated(
        &self,
        _params: ResourceUpdatedNotificationParams,
    ) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_task_status(&self, _task: Task) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_elicitation_complete(
        &self,
        _params: ElicitationCompleteNotificationParams,
    ) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_custom_notification(
        &self,
        _method: String,
        _params: Option<Value>,
    ) -> ClientHandlerFuture<'_> {
        Box::pin(async {})
    }

    fn on_custom_request(
        &self,
        method: String,
        _params: Option<Value>,
    ) -> ClientHandlerResultFuture<'_, Value> {
        Box::pin(async move { Err(GuestError::MethodNotSupported(method)) })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopClientHandler;

impl ClientHandler for NoopClientHandler {}
