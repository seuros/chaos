use chaos_ipc::models::{FunctionCallOutputBody, ResponseItem};
use chaos_ipc::protocol::RolloutItem;
use chaos_traits::catalog::CatalogRegistration;
use mcp_host::prelude::{
    ToolExposure, ToolGroupCatalog, ToolGroupChange, ToolGroupDefinition, ToolGroupError,
    ToolGroupState,
};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};

pub(crate) const SHELL: &str = "shell";
pub(crate) const FILESYSTEM: &str = "filesystem";
pub(crate) const EDITING: &str = "editing";
pub(crate) const AGENTS: &str = "agents";
pub(crate) const SESSION: &str = "session";
pub(crate) const WEB: &str = "web";
pub(crate) const CRON: &str = "cron";
pub(crate) const MCP_MANAGEMENT: &str = "mcp-management";
pub(crate) const RECALL: &str = "recall";
pub(crate) const WORKFLOWS: &str = "workflows";

pub(crate) struct ToolGroupFilter<'a> {
    pub(crate) catalog: &'a ToolGroupCatalog,
    pub(crate) state: &'a ToolGroupState,
}

impl ToolGroupFilter<'_> {
    pub(crate) fn is_visible(&self, tool_name: &str) -> bool {
        self.catalog.is_tool_visible(self.state, tool_name)
    }

    pub(crate) fn is_exposure_visible(&self, exposure: &ToolExposure) -> bool {
        match exposure {
            ToolExposure::Always => true,
            ToolExposure::Groups(groups) => groups
                .iter()
                .any(|group| self.catalog.is_group_enabled(self.state, group)),
        }
    }
}

pub(crate) fn build_catalog() -> Result<ToolGroupCatalog, ToolGroupError> {
    let catalog = ToolGroupCatalog::new();
    for (id, description) in [
        (SHELL, "Command execution and PTY input"),
        (
            FILESYSTEM,
            "Read, search, locate, list, and inspect local files and images",
        ),
        (EDITING, "Patch and repository-editing operations"),
        (
            AGENTS,
            "Spawn, communicate with, wait for, resume, and close subagents",
        ),
        (
            SESSION,
            "Session history, planning, title, compaction, effort, and policy-authorized hooks",
        ),
        (WEB, "Web retrieval and image generation"),
        (CRON, "Recurring job controls"),
        (MCP_MANAGEMENT, "Add and administer configured MCP servers"),
        (WORKFLOWS, "Execute bounded, session-owned tool workflows"),
        (
            RECALL,
            "Search and explicitly store scoped persistent memory",
        ),
    ] {
        catalog.define_group(ToolGroupDefinition::new(id, description, false))?;
    }

    assign(
        &catalog,
        ["enable_tools", "disable_tools"],
        ToolExposure::Always,
    )?;
    assign(
        &catalog,
        ["run_workflow"],
        ToolExposure::groups([WORKFLOWS]),
    )?;
    assign(
        &catalog,
        ["workflow_status", "cancel_workflow"],
        ToolExposure::Always,
    )?;
    assign(
        &catalog,
        [
            "switch_mode",
            "request_user_input",
            "request_permissions",
            "refresh_models",
            "wait_for_machine_recovery",
            "plan",
            "plan_progress",
        ],
        ToolExposure::Always,
    )?;
    assign(
        &catalog,
        [
            "start_attested_review",
            "resume_attested_review",
            "cancel_attested_review",
        ],
        ToolExposure::Always,
    )?;
    assign(
        &catalog,
        [
            "shell",
            "container.exec",
            "local_shell",
            "shell_command",
            "exec_command",
            "write_stdin",
        ],
        ToolExposure::groups([SHELL]),
    )?;
    assign(&catalog, ["apply_patch"], ToolExposure::groups([EDITING]))?;
    assign(
        &catalog,
        [
            "spawn_agent",
            "run_synopsis",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
            "spawn_minions_on_csv",
            "report_minion_job_result",
        ],
        ToolExposure::groups([AGENTS]),
    )?;
    assign(
        &catalog,
        [
            "read_session_history",
            "search_session_history",
            "compaction_control",
            "set_session_title",
            "set_parent_effort",
            "test_sync_tool",
            "hooks_create",
            "hooks_update",
            "hooks_set_enabled",
            "hooks_delete",
            "hooks_preview",
        ],
        ToolExposure::groups([SESSION]),
    )?;
    assign(
        &catalog,
        ["web_search", "image_generation"],
        ToolExposure::groups([WEB]),
    )?;
    assign(&catalog, ["view_image"], ToolExposure::groups([FILESYSTEM]))?;
    assign(
        &catalog,
        crate::tools::handlers::recall::NAMES,
        ToolExposure::groups([RECALL]),
    )?;

    let mut static_tool_names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for registration in inventory::iter::<CatalogRegistration> {
        if !seen.insert(registration.name) {
            continue;
        }
        for tool in (registration.tools)() {
            catalog
                .set_tool_exposure(tool.name.clone(), (registration.tool_exposure)(&tool.name))?;
            static_tool_names.push(tool.name);
        }
    }

    let mut native_tool_names = vec![
        "run_workflow",
        "workflow_status",
        "cancel_workflow",
        "enable_tools",
        "disable_tools",
        "switch_mode",
        "refresh_models",
        "hooks_create",
        "hooks_update",
        "hooks_set_enabled",
        "hooks_delete",
        "hooks_preview",
        "request_user_input",
        "request_permissions",
        "shell",
        "container.exec",
        "local_shell",
        "shell_command",
        "exec_command",
        "write_stdin",
        "apply_patch",
        "spawn_agent",
        "run_synopsis",
        "send_input",
        "resume_agent",
        "wait_agent",
        "close_agent",
        "start_attested_review",
        "resume_attested_review",
        "cancel_attested_review",
        "spawn_minions_on_csv",
        "report_minion_job_result",
        "plan",
        "plan_progress",
        "read_session_history",
        "search_session_history",
        "compaction_control",
        "set_session_title",
        "set_parent_effort",
        "test_sync_tool",
        "web_search",
        "image_generation",
        "view_image",
        "recall_search",
        "recall_store",
        "recall_delete",
        "recall_open",
        "recall_use",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    native_tool_names.extend(static_tool_names);
    catalog.validate_tools(native_tool_names, true)?;
    Ok(catalog)
}

pub(crate) fn new_state(
    catalog: &ToolGroupCatalog,
    enable_all: bool,
) -> Result<ToolGroupState, ToolGroupError> {
    let state = catalog.new_state();
    if enable_all {
        let disabled_groups = catalog.disabled_groups(&state);
        catalog.set_groups_enabled(&state, disabled_groups, true)?;
    }
    Ok(state)
}

/// Restore discovery state, not permissions or tool execution. Replay only
/// acknowledged controls from the full journal so compaction does not erase
/// activation. In particular, native web-search history needs its declaration
/// on the first resumed request, before the model can call enable_tools again.
pub(crate) fn restore_from_rollout(
    catalog: &ToolGroupCatalog,
    state: &ToolGroupState,
    items: &[RolloutItem],
) {
    #[derive(Deserialize)]
    struct Args {
        groups: BTreeSet<String>,
    }

    let available: BTreeSet<_> = catalog.definitions().into_iter().map(|g| g.id).collect();
    let mut pending = HashMap::new();
    for item in items {
        let RolloutItem::ResponseItem(item) = item else {
            continue;
        };
        match item {
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                pending.remove(call_id);
                let enabled = match (namespace.as_deref(), name.as_str()) {
                    (None, "enable_tools") => true,
                    (None, "disable_tools") => false,
                    _ => continue,
                };
                if let Ok(args) = serde_json::from_str::<Args>(arguments)
                    && !args.groups.is_empty()
                {
                    pending.insert(call_id.clone(), (args.groups, enabled));
                }
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                let Some((requested, enabled)) = pending.remove(call_id) else {
                    continue;
                };
                if output.success == Some(false) {
                    continue;
                }
                let FunctionCallOutputBody::Text(text) = &output.body else {
                    continue;
                };
                let Ok(change) = serde_json::from_str::<ToolGroupChange>(text) else {
                    continue;
                };
                let acknowledged: BTreeSet<_> = change
                    .changed_groups
                    .into_iter()
                    .chain(change.unchanged_groups)
                    .collect();
                if acknowledged != requested {
                    continue;
                }
                // A removed group must not prevent restoring other groups
                // from a valid control recorded by an older catalog.
                let groups = acknowledged.intersection(&available).cloned();
                if let Err(error) = catalog.set_groups_enabled(state, groups, enabled) {
                    tracing::warn!(%error, "could not restore tool group activation");
                }
            }
            _ => {}
        }
    }
}

/// Side-effect-free preflight, before a capability group can prepare assets.
pub(crate) fn validate_groups(
    catalog: &ToolGroupCatalog,
    groups: &[String],
) -> Result<(), ToolGroupError> {
    let available: Vec<_> = catalog
        .definitions()
        .into_iter()
        .map(|group| group.id)
        .collect();
    for group in groups {
        if !available.contains(group) {
            return Err(ToolGroupError::UnknownGroup {
                group: group.clone(),
                available,
            });
        }
    }
    Ok(())
}

fn assign<const N: usize>(
    catalog: &ToolGroupCatalog,
    names: [&str; N],
    exposure: ToolExposure,
) -> Result<(), ToolGroupError> {
    for name in names {
        catalog.set_tool_exposure(name, exposure.clone())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
