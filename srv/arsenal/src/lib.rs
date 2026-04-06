pub mod tools;

use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogToolDriver;
use chaos_traits::catalog::CatalogToolDriverFuture;
use chaos_traits::catalog::CatalogToolRequest;
use chaos_traits::catalog::CatalogToolResult;
use chaos_traits::catalog::tool_infos_to_catalog_tools;
use mcp_host::prelude::*;
use std::sync::Arc;

struct ArsenalToolDriver;

impl CatalogToolDriver for ArsenalToolDriver {
    fn call_tool(&self, request: CatalogToolRequest) -> CatalogToolDriverFuture<'_> {
        Box::pin(async move {
            let result = match request.tool_name.as_str() {
                "read_file" => tools::read_file::execute(&request.arguments).await,
                "grep_files" => tools::grep_files::execute(&request.arguments).await,
                "list_dir" => tools::list_dir::execute(&request.arguments).await,
                other => Err(format!("unknown arsenal tool: {other}")),
            }?;
            Ok(CatalogToolResult {
                output: result,
                success: Some(true),
                effects: Vec::new(),
            })
        })
    }
}

fn arsenal_tool_driver() -> Arc<dyn CatalogToolDriver> {
    Arc::new(ArsenalToolDriver)
}

inventory::submit! {
    CatalogRegistration {
        name: "arsenal",
        tools: || tool_infos_to_catalog_tools(tools::tool_infos()),
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
        tool_driver: Some(arsenal_tool_driver),
    }
}

/// Shared server state for all built-in Chaos tools.
///
/// Future: will hold cwd, sandbox policy, session state.
pub struct ChaosServer;

/// Shared server execution context for all built-in Chaos tools.
pub type ChaosCtx<'a> = Ctx<'a>;
