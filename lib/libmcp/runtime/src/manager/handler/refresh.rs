use super::{Arc, Duration, McpCatalogSink, McpSession, StdRwLock, ToolFilter, ToolInfo, warn};
use crate::manager::client::list_tools_for_session_uncached;
use crate::manager::filter::store_managed_tools;

/// Re-list tools, apply the filter, then publish the filtered set to the catalog.
pub(super) async fn refresh_tools(
    server_name: &str,
    session: &McpSession,
    tool_timeout: Duration,
    tool_filter: &ToolFilter,
    tools_arc: &Arc<StdRwLock<Vec<ToolInfo>>>,
    catalog: &dyn McpCatalogSink,
) {
    match list_tools_for_session_uncached(server_name, session, Some(tool_timeout)).await {
        Ok(tools) => {
            store_managed_tools(tool_filter, tools_arc, tools);
            let catalog_tools = tools_arc.read().ok().map(|store| {
                store
                    .iter()
                    .map(crate::catalog_conv::mcp_tool_info_to_catalog_tool)
                    .collect()
            });
            if let Some(catalog_tools) = catalog_tools {
                catalog.unregister_mcp_tools(server_name);
                catalog.register_mcp_tools(server_name, catalog_tools);
            }
        }
        Err(err) => {
            warn!("Failed to refresh tool list for '{server_name}': {err}");
        }
    }
}

/// Re-list resources and resource templates, replacing them in the catalog.
pub(super) async fn refresh_resources(
    server_name: &str,
    session: &McpSession,
    catalog: &dyn McpCatalogSink,
) {
    let resources = match session.list_resources().await {
        Ok(list) => list
            .into_iter()
            .map(crate::catalog_conv::mcp_resource_to_catalog)
            .collect(),
        Err(err) => {
            warn!("Failed to refresh resource list for '{server_name}': {err}");
            return;
        }
    };

    let templates = match session.list_resource_templates().await {
        Ok(list) => list
            .into_iter()
            .map(crate::catalog_conv::mcp_resource_template_to_catalog)
            .collect(),
        Err(err) => {
            warn!("Failed to refresh resource template list for '{server_name}': {err}");
            Vec::new()
        }
    };

    // `unregister_mcp` would also drop tools and prompts.
    catalog.unregister_mcp_resources(server_name);
    catalog.register_mcp_resources(server_name, resources, templates);
}

/// Re-list prompts and replace them in the catalog.
pub(super) async fn refresh_prompts(
    server_name: &str,
    session: &McpSession,
    catalog: &dyn McpCatalogSink,
) {
    let prompts = match session.list_prompts().await {
        Ok(result) => result
            .into_iter()
            .map(crate::catalog_conv::mcp_prompt_to_catalog)
            .collect(),
        Err(err) => {
            warn!("Failed to refresh prompt list for '{server_name}': {err}");
            return;
        }
    };

    catalog.unregister_mcp_prompts(server_name);
    catalog.register_mcp_prompts(server_name, prompts);
}
