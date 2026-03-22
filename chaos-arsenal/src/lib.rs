pub mod tools;

use mcp_host::prelude::*;

/// Shared server state for all built-in Chaos tools.
///
/// Future: will hold cwd, sandbox policy, session state.
pub struct ChaosServer;

/// Lightweight context wrapper — mirrors the Prometheus pattern.
#[derive(Clone)]
pub struct ChaosCtx<'a> {
    inner: Ctx<'a>,
}

impl<'a> FromExecutionContext<'a> for ChaosCtx<'a> {
    fn from_execution_context(ctx: &'a ExecutionContext<'a>) -> Self {
        Self {
            inner: Ctx::from_execution_context(ctx),
        }
    }
}

impl<'a> std::ops::Deref for ChaosCtx<'a> {
    type Target = Ctx<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
