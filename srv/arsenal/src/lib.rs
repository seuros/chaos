pub mod tools;

use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogTool;
use mcp_host::prelude::*;

inventory::submit! {
    CatalogRegistration {
        name: "arsenal",
        tools: || {
            tools::tool_infos().into_iter().map(|info| CatalogTool {
                name: info.name,
                description: info.description.unwrap_or_default(),
                input_schema: info.input_schema,
                annotations: info.annotations
                    .and_then(|a| serde_json::to_value(a).ok()),
            }).collect()
        },
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
    }
}

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
