use super::groups::RootControl;
use super::*;

impl WorkflowScope {
    /// Host-only root dispatch. Agent leaves still use the ordinary agent
    /// authority, admission, accounting and fresh-child cleanup paths.
    pub async fn run_synopsis(&self, ctx: ExecutionContext<'_>) -> Result<ToolOutput, ToolError> {
        self.dispatch_root(ctx, RootControl::Synopsis).await
    }
}
