mod adapters;
mod config;
mod registry;
mod schemas;
mod tool_builders;

#[cfg(test)]
use serde_json::json;

#[cfg(test)]
pub(crate) use crate::client_common::tools::ToolSpec;
#[cfg(test)]
pub(crate) use crate::features::Feature;
#[cfg(test)]
pub(crate) use crate::features::Features;
#[cfg(test)]
pub(crate) use crate::models_manager::collaboration_mode_presets::CollaborationModesConfig;
#[cfg(test)]
pub(crate) use crate::tools::handlers::PLAN_TOOL;
#[cfg(test)]
pub(crate) use crate::tools::handlers::apply_patch::create_apply_patch_freeform_tool;
#[cfg(test)]
pub(crate) use chaos_ipc::config_types::WebSearchConfig;
#[cfg(test)]
pub(crate) use chaos_ipc::config_types::WebSearchMode;
#[cfg(test)]
pub(crate) use chaos_ipc::models::VIEW_IMAGE_TOOL_NAME;
#[cfg(test)]
pub(crate) use chaos_ipc::openai_models::ConfigShellToolType;
#[cfg(test)]
pub(crate) use chaos_ipc::openai_models::WebSearchToolType;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::SandboxPolicy;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::SessionSource;
#[cfg(test)]
pub(crate) use chaos_ipc::protocol::SubAgentSource;
#[cfg(test)]
pub(crate) use registry::WEB_SEARCH_CONTENT_TYPES;
#[cfg(test)]
pub(crate) use std::collections::BTreeMap;
#[cfg(test)]
pub(crate) use std::collections::HashMap;

pub(crate) use adapters::ApplyPatchToolArgs;
pub(crate) use adapters::annotation_labels;
#[cfg(test)]
pub(crate) use adapters::annotation_suffix;
pub use adapters::create_tools_json_for_responses_api;
#[cfg(test)]
pub(crate) use adapters::dynamic_tool_to_model_tool;
#[cfg(test)]
pub(crate) use adapters::mcp_tool_to_deferred_model_tool;
#[cfg(test)]
pub(crate) use adapters::mcp_tool_to_model_tool;
pub(crate) use config::ToolsConfig;
pub(crate) use config::ToolsConfigParams;
#[cfg(test)]
pub(crate) use registry::build_specs;
pub(crate) use registry::build_specs_with_discoverable_tools;
pub(crate) use tool_builders::{create_call_mcp_tool_async_tool, create_cancel_mcp_task_tool};
#[cfg(test)]
pub(crate) use tool_builders::{
    create_close_agent_tool, create_exec_command_tool, create_request_permissions_tool,
    create_request_user_input_tool, create_resume_agent_tool, create_send_input_tool,
    create_shell_command_tool, create_shell_tool, create_spawn_agent_tool, create_view_image_tool,
    create_wait_agent_tool, create_write_stdin_tool,
};

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
