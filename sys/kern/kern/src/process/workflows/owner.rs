//! External-session ownership, independent of any one native operation process.

use super::*;
use mcp_host::prelude::ToolGroupState;

#[derive(Clone)]
pub struct WorkflowOwner(pub(super) Arc<Owner>);

pub(super) struct Owner {
    pub(super) session_id: String,
    pub(super) registry: RunRegistry,
    pub(super) closed: CancellationToken,
    groups: ToolGroupState,
    scheduler: std::sync::OnceLock<crate::tools::parallel::ToolSchedulerActor>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.closed.cancel();
        self.registry.shutdown();
    }
}

impl WorkflowOwner {
    /// Trusted embedding boundary. Retain exactly one owner per SDK session.
    pub fn new(session: &mcp_host::server::session::Session) -> Self {
        Self(Arc::new(Owner {
            session_id: session.id().into(),
            registry: RunRegistry::default(),
            closed: CancellationToken::new(),
            groups: ToolGroupState::default(),
            scheduler: std::sync::OnceLock::new(),
        }))
    }

    pub(super) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn registry(&self) -> &RunRegistry {
        &self.0.registry
    }

    fn groups(&self) -> &ToolGroupState {
        &self.0.groups
    }

    pub(super) fn check(&self, ctx: &ExecutionContext<'_>) -> Result<(), ToolError> {
        if ctx.session.id() != self.0.session_id {
            return Err(ToolError::Execution(
                "workflow owner belongs to another session".into(),
            ));
        }
        if self.0.closed.is_cancelled() {
            return Err(ToolError::Execution("workflow owner has closed".into()));
        }
        Ok(())
    }

    /// Inspect only this SDK session's retained execution.
    pub fn status(&self, ctx: ExecutionContext<'_>) -> Result<ToolOutput, ToolError> {
        self.snapshot(ctx, false)
    }

    /// Cancellation is not terminal until the executor finishes cleanup.
    pub fn cancel(&self, ctx: ExecutionContext<'_>) -> Result<ToolOutput, ToolError> {
        self.snapshot(ctx, true)
    }

    fn snapshot(&self, ctx: ExecutionContext<'_>, cancel: bool) -> Result<ToolOutput, ToolError> {
        self.check(&ctx)?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            run_id: RunId,
        }
        let args: Args = serde_json::from_value(ctx.params)
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        let snapshot = if cancel {
            self.0.registry.cancel(args.run_id)
        } else {
            self.0.registry.status(args.run_id)
        }
        .map_err(|error| ToolError::Execution(error.to_string()))?;
        ToolOutput::structured(snapshot).map_err(|error| ToolError::Internal(error.to_string()))
    }

    /// A bounded metadata-only dashboard page; payload details require status.
    pub fn dashboard(&self, ctx: ExecutionContext<'_>) -> Result<ToolOutput, ToolError> {
        self.check(&ctx)?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            #[serde(default)]
            offset: usize,
        }
        let args: Args = serde_json::from_value(ctx.params)
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        ToolOutput::structured(self.0.registry.summaries(args.offset))
            .map_err(|error| ToolError::Internal(error.to_string()))
    }

    /// Call only on SDK session expiry/DELETE/server shutdown, not SSE loss.
    pub fn close(&self) {
        self.0.closed.cancel();
        self.0.registry.shutdown();
    }
}

impl Session {
    /// Operation processes have independent turns/event queues, but their
    /// owner's mutation/read scheduling boundary must remain session-wide.
    pub(crate) fn tool_scheduler(&self) -> &crate::tools::parallel::ToolSchedulerActor {
        match self.services.external_workflow_owner.get() {
            Some(owner) => owner
                .0
                .scheduler
                .get_or_init(|| self.services.tool_scheduler.clone()),
            None => &self.services.tool_scheduler,
        }
    }

    /// Bound external processes share activation across all of their owner's
    /// operation roots. Native sessions retain their own activation state.
    pub(crate) fn tool_group_state(&self) -> &ToolGroupState {
        self.services
            .external_workflow_owner
            .get()
            .map_or(&self.services.tool_group_state, WorkflowOwner::groups)
    }

    pub(crate) fn workflow_runs(&self) -> &RunRegistry {
        self.services
            .external_workflow_owner
            .get()
            .map_or(&self.services.workflow_runs, WorkflowOwner::registry)
    }
}
