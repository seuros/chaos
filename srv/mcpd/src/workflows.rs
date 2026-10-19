//! One native operation process per root request; one run owner per SDK session.
//! Never reuse an event receiver or approval requester between root calls.

mod events;
mod schema;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_kern::{Process, ProcessTable, WorkflowScope};
use mcp_host::prelude::*;
use mcp_host::protocol::types::{TaskSupport, ToolExecution};
use mcp_host::registry::tools::{Tool, ToolFuture};
use serde_json::{Map, Value};

use crate::chaos_tool::ChaosMcpServer;
use crate::outgoing_message::OutgoingMessageSender;
use crate::workflow_app;

#[derive(Clone, Copy)]
enum Operation {
    Run,
    Synopsis,
    Status,
    Cancel,
    Dashboard,
    Enable,
    Disable,
}

struct Endpoint {
    service: Arc<ChaosMcpServer>,
    operation: Operation,
}

pub(crate) fn register(
    server: &Arc<Server>,
    service: Arc<ChaosMcpServer>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for operation in [
        Operation::Run,
        Operation::Synopsis,
        Operation::Status,
        Operation::Cancel,
        Operation::Dashboard,
        Operation::Enable,
        Operation::Disable,
    ] {
        server.tool_registry().register(Endpoint {
            service: service.clone(),
            operation,
        })?;
    }
    workflow_app::resource_router()?.register_all(server.resource_manager(), service)?;
    Ok(())
}

impl Tool for Endpoint {
    fn name(&self) -> &str {
        match self.operation {
            Operation::Run => "run_workflow",
            Operation::Synopsis => "run_synopsis",
            Operation::Status => "workflow_status",
            Operation::Cancel => workflow_app::CANCEL_TOOL,
            Operation::Dashboard => workflow_app::DASHBOARD_TOOL,
            Operation::Enable => "enable_tools",
            Operation::Disable => "disable_tools",
        }
    }

    fn description(&self) -> Option<&str> {
        Some(schema::description(self.operation))
    }

    fn input_schema(&self) -> Value {
        schema::input(self.operation)
    }

    fn execution(&self) -> Option<ToolExecution> {
        matches!(self.operation, Operation::Run | Operation::Synopsis).then_some(ToolExecution {
            task_support: Some(TaskSupport::Optional),
        })
    }

    fn meta(&self) -> Option<Map<String, Value>> {
        if matches!(self.operation, Operation::Enable | Operation::Disable) {
            return None;
        }
        let ui = if matches!(self.operation, Operation::Dashboard) {
            workflow_app::dashboard_tool_ui()
        } else {
            workflow_app::linked_tool_ui()
        };
        // The URI is a compile-time constant; registration validates metadata.
        ui.map(ToolUi::into_meta).ok()
    }

    fn execute<'a>(&'a self, ctx: ExecutionContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let state = self.service.session_state(ctx.session)?;
            match self.operation {
                Operation::Status => state.workflow_owner.status(ctx).map(with_fallback),
                Operation::Cancel => state.workflow_owner.cancel(ctx).map(with_fallback),
                Operation::Dashboard => {
                    if !ctx
                        .client_capabilities()
                        .is_some_and(ClientCapabilities::supports_mcp_apps)
                    {
                        return Err(ToolError::Execution(workflow_app::TEXT_FALLBACK.into()));
                    }
                    state.workflow_owner.dashboard(ctx).map(with_fallback)
                }
                operation => {
                    // Revocation controls must remain available with all four
                    // run slots busy; controls have their own single admission.
                    let control = matches!(operation, Operation::Enable | Operation::Disable);
                    let _control = if control {
                        Some(state.group_controls.try_lock().map_err(|_| {
                            ToolError::Execution("group control is already active".into())
                        })?)
                    } else {
                        None
                    };
                    let _admission = if control {
                        None
                    } else {
                        Some(
                            state
                                .workflow_admission
                                .clone()
                                .try_acquire_owned()
                                .map_err(|_| {
                                    ToolError::Execution(
                                        "session workflow admission is full or closed".into(),
                                    )
                                })?,
                        )
                    };
                    let outgoing = Arc::new(OutgoingMessageSender::new(
                        self.service.mcp_server.clone(),
                        ctx.session.id().into(),
                        ctx.client_requester().cloned(),
                        state.url_elicitations.clone(),
                    ));
                    let request_id = ctx.request_id().cloned().ok_or_else(|| {
                        ToolError::Execution("workflow requires an originating MCP request".into())
                    })?;
                    // Optional-task SDK handlers have no registry timeout.
                    // Our deadline covers admission/startup as well as execution.
                    let limit = schema::deadline(operation, &ctx.params)?;
                    let execute = async {
                        let new = state
                            .workflow_process_table
                            .start_process(self.service.config.as_ref().clone())
                            .await
                            .map_err(execution)?;
                        let (id, process, configured) = new.into_parts();
                        let mut cleanup = OperationGuard {
                            id,
                            process: process.clone(),
                            table: state.workflow_process_table.clone(),
                            scope: None,
                            finished: false,
                        };
                        outgoing
                            .send_event_as_notification(
                                &chaos_ipc::protocol::Event {
                                    id: String::new(),
                                    msg: chaos_ipc::protocol::EventMsg::SessionConfigured(
                                        configured,
                                    ),
                                },
                                Some(crate::outgoing_message::OutgoingNotificationMeta {
                                    request_id: Some(request_id.clone()),
                                    process_id: Some(id),
                                }),
                            )
                            .await;
                        let scope = process
                            .open_workflows_for(&state.workflow_owner)
                            .await
                            .map_err(execution)?;
                        cleanup.scope = Some(scope.clone());
                        let result = {
                            let run = async {
                                match operation {
                                    Operation::Run => scope.run(ctx).await,
                                    Operation::Synopsis => scope.run_synopsis(ctx).await,
                                    Operation::Enable => scope.configure_groups(ctx, true).await,
                                    Operation::Disable => scope.configure_groups(ctx, false).await,
                                    _ => unreachable!(),
                                }
                            };
                            tokio::pin!(run);
                            tokio::select! {
                                biased;
                                _ = state.cancelled() => {
                                    Err(ToolError::Execution("MCP session closed".into()))
                                },
                                result = &mut run => result,
                                result = events::pump(id, process, outgoing, request_id) => {
                                    result.and(Err(ToolError::Execution(
                                        "workflow event stream ended".into()
                                    )))
                                }
                            }
                        };
                        cleanup.finish().await;
                        result.map(|output| {
                            if control {
                                output
                            } else {
                                with_fallback(output)
                            }
                        })
                    };
                    tokio::time::timeout(limit, execute).await.map_err(|_| {
                        ToolError::Execution("workflow request deadline expired".into())
                    })?
                }
            }
        })
    }
}

/// A dropped SDK task/request must still revoke its native permissions and
/// submit bounded shutdown. Failed shutdowns remain visible to session cleanup.
struct OperationGuard {
    id: ProcessId,
    process: Arc<Process>,
    table: Arc<ProcessTable>,
    scope: Option<WorkflowScope>,
    finished: bool,
}

impl OperationGuard {
    async fn finish(&mut self) {
        if let Some(scope) = self.scope.take() {
            scope.close();
        }
        if matches!(
            tokio::time::timeout(Duration::from_secs(5), self.process.shutdown_and_wait()).await,
            Ok(Ok(()))
        ) {
            self.table.remove_process(&self.id).await;
            self.finished = true;
        }
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        if let Some(scope) = self.scope.take() {
            scope.close();
        }
        if self.finished {
            return;
        }
        let table = self.table.clone();
        let process = self.process.clone();
        let id = self.id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if matches!(
                    tokio::time::timeout(Duration::from_secs(5), process.shutdown_and_wait()).await,
                    Ok(Ok(()))
                ) {
                    table.remove_process(&id).await;
                }
            });
        }
    }
}

fn execution(error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution(error.to_string())
}

fn with_fallback(output: ToolOutput) -> ToolOutput {
    match output {
        ToolOutput::Structured(payload) => {
            let value = Value::Object(payload);
            let summary = workflow_app::dashboard_text(&value);
            let Value::Object(mut payload) = value else {
                unreachable!()
            };
            payload.insert("text_fallback".into(), Value::String(summary));
            ToolOutput::json(payload)
        }
        output => output,
    }
}
