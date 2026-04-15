use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::error::GuestError;
use crate::protocol::CreateElicitationRequest;
use crate::protocol::CreateElicitationResponse;
use crate::protocol::CreateMessageRequest;
use crate::protocol::CreateMessageResponse;
use crate::protocol::ElicitationCompleteNotificationParams;
use crate::protocol::ListRootsResult;
use crate::protocol::LogMessageNotificationParams;
use crate::protocol::ProgressNotificationParams;
use crate::protocol::ResourceUpdatedNotificationParams;
use crate::protocol::Task;

pub type ClientHandlerFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
pub type ClientHandlerResultFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, GuestError>> + Send + 'a>>;

macro_rules! noop_notification {
    ($name:ident) => {
        fn $name(&self) -> ClientHandlerFuture<'_> {
            Box::pin(async {})
        }
    };
    ($name:ident, $param_ty:ty) => {
        fn $name(&self, _: $param_ty) -> ClientHandlerFuture<'_> {
            Box::pin(async {})
        }
    };
    ($name:ident, $param1_ty:ty, $param2_ty:ty) => {
        fn $name(&self, _: $param1_ty, _: $param2_ty) -> ClientHandlerFuture<'_> {
            Box::pin(async {})
        }
    };
}

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

    noop_notification!(on_log_message, LogMessageNotificationParams);
    noop_notification!(on_progress, ProgressNotificationParams);
    noop_notification!(on_tools_list_changed);
    noop_notification!(on_resources_list_changed);
    noop_notification!(on_prompts_list_changed);
    noop_notification!(on_roots_list_changed);
    noop_notification!(on_resource_updated, ResourceUpdatedNotificationParams);
    noop_notification!(on_task_status, Task);
    noop_notification!(
        on_elicitation_complete,
        ElicitationCompleteNotificationParams
    );
    noop_notification!(on_custom_notification, String, Option<Value>);

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
