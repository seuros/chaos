//! Narrow MCP dispatch attachment. Host-only idle admission is serialized by the
//! kernel; caller data cannot grant permissions, enable tools or select providers.

mod admission;
mod groups;
mod owner;
mod synopsis;
use super::Process;
use crate::chaos::Session;
use crate::error::ChaosErr;
use crate::tools::context::{ToolInvocation, ToolPayload};
use crate::tools::workflows::{RunArgs, executor};
use crate::turn_diff_tracker::TurnDiffTracker;
pub(crate) use admission::WorkflowControl;
use chaos_synopsis::runs::{RunId, RunRegistry};
use mcp_host::prelude::{ExecutionContext, ToolError, ToolOutput};
pub use owner::WorkflowOwner;
use serde::Deserialize;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// An attachment owned by one external MCP session and one existing native
/// turn. Clones share the same run budget, status and cancellation authority.
///
/// The server must authorize access to the Process before creating this handle,
/// retain it in that MCP session (never a global catalog), and call `close` on
/// session expiry/DELETE. A temporary HTTP stream loss is not session expiry.
/// `open_workflows` owns a dedicated model-free operation turn; closing its
/// scope ends that operation. `attach_workflows` borrows an existing turn.
/// Neither silently changes mode, sandbox, approval or provider defaults.
#[derive(Clone)]
pub struct WorkflowScope(Arc<Scope>);

struct Scope {
    session_id: String,
    invocation: ToolInvocation,
    registry: RunRegistry,
    lifetime: CancellationToken,
    /// Present only for a host-admitted, model-free operation turn.
    operation: Option<CancellationToken>,
    owner: Option<WorkflowOwner>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.lifetime.cancel();
        if let Some(operation) = &self.operation {
            operation.cancel();
        }
        if self.owner.is_none() {
            self.registry.shutdown();
        }
    }
}

impl Process {
    /// Admit one request on a dedicated idle process, sharing its external
    /// session's run budget with sibling operation processes.
    pub async fn open_workflows_for(
        &self,
        owner: &WorkflowOwner,
    ) -> Result<WorkflowScope, ChaosErr> {
        self.chaos
            .session
            .services
            .workflow_control
            .open_owned(owner.clone())
            .await
    }

    /// Open a dedicated, model-free operation turn on an idle process.
    /// Admission is serialized with owner submissions; busy/recovering processes
    /// are refused rather than interrupted. Keep one scope per owning MCP
    /// session on its dedicated workflow process.
    pub async fn open_workflows(
        &self,
        external_session: &mcp_host::server::session::Session,
    ) -> Result<WorkflowScope, ChaosErr> {
        self.chaos
            .session
            .services
            .workflow_control
            .open(external_session.id())
            .await
    }

    /// Attach an already-authorized external MCP session to the current native
    /// turn. Refuses idle processes; no prompt or provider call is involved.
    /// Finishing/interruption of that turn revokes the attachment.
    pub async fn attach_workflows(
        &self,
        external_session: &mcp_host::server::session::Session,
    ) -> Result<WorkflowScope, ChaosErr> {
        attach(&self.chaos.session, external_session.id()).await
    }
}

async fn attach(session: &Arc<Session>, session_id: &str) -> Result<WorkflowScope, ChaosErr> {
    attach_with_operation(session, session_id, None, None).await
}

async fn attach_with_operation(
    session: &Arc<Session>,
    session_id: &str,
    operation: Option<CancellationToken>,
    owner: Option<WorkflowOwner>,
) -> Result<WorkflowScope, ChaosErr> {
    let active = session.active_turn.lock().await;
    let task = active
        .as_ref()
        .and_then(|turn| turn.tasks.first())
        .map(|(_, task)| task)
        .ok_or_else(|| {
            ChaosErr::InvalidRequest("workflow attachment requires an active turn".into())
        })?;
    if task.cancellation_token.is_cancelled() || task.handle.is_finished() {
        return Err(ChaosErr::InvalidRequest("the active turn is ending".into()));
    }
    // Normal completion and interruption both revoke the task token before
    // removing turn permissions. No detached waiter or unbuffered Notify race.
    let lifetime = task.cancellation_token.child_token();
    let invocation = ToolInvocation {
        session: session.clone(),
        turn: task.turn_context.clone(),
        tracker: Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
        call_id: format!("external-workflow/{}", uuid::Uuid::new_v4()),
        tool_name: "run_workflow".into(),
        tool_namespace: None,
        payload: ToolPayload::Function {
            arguments: "{}".into(),
        },
    };
    Ok(WorkflowScope(Arc::new(Scope {
        session_id: session_id.into(),
        invocation,
        registry: owner
            .as_ref()
            .map_or_else(RunRegistry::default, |owner| owner.registry().clone()),
        lifetime,
        operation,
        owner,
    })))
}

impl WorkflowScope {
    fn check_owner(&self, ctx: &ExecutionContext<'_>) -> Result<(), ToolError> {
        if ctx.session.id() != self.0.session_id {
            return Err(ToolError::Execution(
                "workflow scope belongs to another session".into(),
            ));
        }
        Ok(())
    }

    /// Execute the bounded tree in `ctx.params`. Each call uses the normal
    /// kernel dispatcher; interactions use only this request's negotiated MCP
    /// requester, including its task route. Caller `_meta` never grants power.
    pub async fn run(&self, ctx: ExecutionContext<'_>) -> Result<ToolOutput, ToolError> {
        self.check_owner(&ctx)?;
        let args: RunArgs = serde_json::from_value(ctx.params.clone())
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        let snapshot = executor::run(
            self.0.invocation.clone(),
            args,
            &self.0.registry,
            executor::Origin::external(ctx),
            self.0.lifetime.clone(),
        )
        .await
        .map_err(|error| ToolError::Execution(error.to_string()))?;
        ToolOutput::structured(snapshot).map_err(|error| ToolError::Internal(error.to_string()))
    }

    /// Inspect an owned run even after workflows are disabled or the turn ends.
    pub fn status(&self, ctx: ExecutionContext<'_>) -> Result<ToolOutput, ToolError> {
        self.snapshot(ctx, false)
    }

    /// Request cancellation. The snapshot is terminal only after cleanup.
    pub fn cancel(&self, ctx: ExecutionContext<'_>) -> Result<ToolOutput, ToolError> {
        self.snapshot(ctx, true)
    }

    fn snapshot(&self, ctx: ExecutionContext<'_>, cancel: bool) -> Result<ToolOutput, ToolError> {
        self.check_owner(&ctx)?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            run_id: RunId,
        }
        let args: Args = serde_json::from_value(ctx.params)
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        let result = if cancel {
            self.0.registry.cancel(args.run_id)
        } else {
            self.0.registry.status(args.run_id)
        }
        .map_err(|error| ToolError::Execution(error.to_string()))?;
        ToolOutput::structured(result).map_err(|error| ToolError::Internal(error.to_string()))
    }

    /// Session teardown revokes work even if an executing request still holds
    /// a clone. Do not call this merely because an SSE stream disconnected.
    pub fn close(&self) {
        self.0.lifetime.cancel();
        if let Some(operation) = &self.0.operation {
            operation.cancel();
        }
        if self.0.owner.is_none() {
            self.0.registry.shutdown();
        }
    }
}

#[cfg(test)]
mod tests;
