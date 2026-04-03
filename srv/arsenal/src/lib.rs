pub mod tools;

use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::tool_infos_to_catalog_tools;
use mcp_host::prelude::*;

inventory::submit! {
    CatalogRegistration {
        name: "arsenal",
        tools: || tool_infos_to_catalog_tools(tools::tool_infos()),
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
    }
}

/// Shared server state for all built-in Chaos tools.
///
/// Future: will hold cwd, sandbox policy, session state.
pub struct ChaosServer;

/// Shared server execution context for all built-in Chaos tools.
pub type ChaosCtx<'a> = Ctx<'a>;
