use std::collections::BTreeSet;
use std::collections::HashMap;

use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::ResponseItem;

use crate::mcp_connection_manager::ToolInfo;
use crate::plugins::PluginCapabilitySummary;
use crate::plugins::render_explicit_plugin_instructions;

pub(crate) fn build_plugin_injections(
    mentioned_plugins: &[PluginCapabilitySummary],
    mcp_tools: &HashMap<String, ToolInfo>,
) -> Vec<ResponseItem> {
    if mentioned_plugins.is_empty() {
        return Vec::new();
    }

    // Turn each explicit plugin mention into a developer hint that points the
    // model at the plugin's visible MCP servers and skill prefix.
    mentioned_plugins
        .iter()
        .filter_map(|plugin| {
            let available_mcp_servers = mcp_tools
                .values()
                .filter(|tool| {
                    tool.plugin_display_names
                        .iter()
                        .any(|plugin_name| plugin_name == &plugin.display_name)
                })
                .map(|tool| tool.server_name.clone())
                .collect::<BTreeSet<String>>()
                .into_iter()
                .collect::<Vec<_>>();
            render_explicit_plugin_instructions(plugin, &available_mcp_servers, &[])
                .map(DeveloperInstructions::new)
                .map(ResponseItem::from)
        })
        .collect()
}
