use chaos_traits::catalog::CatalogRegistration;
use mcp_host::prelude::{
    ToolExposure, ToolGroupCatalog, ToolGroupDefinition, ToolGroupError, ToolGroupState,
};

pub(crate) const GIT: &str = "git";
pub(crate) const GIT_WRITE: &str = "git-write";
pub(crate) const SHELL: &str = "shell";
pub(crate) const FILESYSTEM: &str = "filesystem";
pub(crate) const EDITING: &str = "editing";
pub(crate) const AGENTS: &str = "agents";
pub(crate) const SESSION: &str = "session";
pub(crate) const WEB: &str = "web";
pub(crate) const CRON: &str = "cron";
pub(crate) const MCP_MANAGEMENT: &str = "mcp-management";

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
        (GIT, "Gix-backed read-only repository inspection"),
        (GIT_WRITE, "Gix-backed repository mutation operations"),
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
            "Session history, planning, title, compaction, and effort controls",
        ),
        (WEB, "Web retrieval and image generation"),
        (CRON, "Recurring job controls"),
        (MCP_MANAGEMENT, "Add and administer configured MCP servers"),
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
        ["switch_mode", "request_user_input", "request_permissions"],
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
            "start_attested_review",
            "resume_attested_review",
            "cancel_attested_review",
            "spawn_minions_on_csv",
            "report_minion_job_result",
        ],
        ToolExposure::groups([AGENTS]),
    )?;
    assign(
        &catalog,
        [
            "update_plan",
            "read_session_history",
            "search_session_history",
            "compaction_control",
            "set_session_title",
            "set_parent_effort",
            "test_sync_tool",
        ],
        ToolExposure::groups([SESSION]),
    )?;
    assign(
        &catalog,
        ["web_search", "image_generation"],
        ToolExposure::groups([WEB]),
    )?;
    assign(&catalog, ["view_image"], ToolExposure::groups([FILESYSTEM]))?;

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
        "enable_tools",
        "disable_tools",
        "switch_mode",
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
        "update_plan",
        "read_session_history",
        "search_session_history",
        "compaction_control",
        "set_session_title",
        "set_parent_effort",
        "test_sync_tool",
        "web_search",
        "image_generation",
        "view_image",
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
mod tests {
    use super::*;

    #[test]
    fn catalog_is_strict_and_defaults_operational_groups_off() {
        let catalog = build_catalog().expect("tool group catalog");
        let state = new_state(&catalog, false).expect("tool group state");

        assert!(catalog.is_tool_visible(&state, "enable_tools"));
        assert!(!catalog.is_tool_visible(&state, "git_commit"));
        assert!(!catalog.is_tool_visible(&state, "read_file"));

        catalog
            .set_groups_enabled(&state, [GIT, GIT_WRITE, FILESYSTEM], true)
            .expect("enable groups");
        assert!(catalog.is_tool_visible(&state, "git_commit"));
        assert!(catalog.is_tool_visible(&state, "git_branch"));
        assert!(catalog.is_tool_visible(&state, "read_file"));
    }

    #[test]
    fn clamp_state_starts_with_operational_groups_enabled() {
        let catalog = build_catalog().expect("tool group catalog");
        let state = new_state(&catalog, true).expect("tool group state");

        assert!(catalog.disabled_groups(&state).is_empty());
        assert!(catalog.is_tool_visible(&state, "exec_command"));
        assert!(catalog.is_tool_visible(&state, "read_file"));
        assert!(catalog.is_tool_visible(&state, "apply_patch"));
    }

    #[test]
    fn git_read_and_write_groups_are_independent() {
        let catalog = build_catalog().expect("tool group catalog");
        let state = catalog.new_state();

        catalog
            .set_groups_enabled(&state, [GIT], true)
            .expect("enable git reads");
        assert!(catalog.is_tool_visible(&state, "git_status"));
        assert!(!catalog.is_tool_visible(&state, "git_commit"));
        assert!(!catalog.is_tool_visible(&state, "git_branch"));

        catalog
            .set_groups_enabled(&state, [GIT_WRITE], true)
            .expect("enable git writes");
        assert!(catalog.is_tool_visible(&state, "git_commit"));
        assert!(catalog.is_tool_visible(&state, "git_branch"));
    }

    #[test]
    fn states_do_not_inherit_activation() {
        let catalog = build_catalog().expect("tool group catalog");
        let parent = catalog.new_state();
        let child = catalog.new_state();
        catalog
            .set_groups_enabled(&parent, [GIT], true)
            .expect("enable git");

        assert!(catalog.is_tool_visible(&parent, "git_status"));
        assert!(!catalog.is_tool_visible(&child, "git_status"));
    }

    #[test]
    fn activation_is_atomic_and_idempotent_for_chaos_groups() {
        let catalog = build_catalog().expect("tool group catalog");
        let state = catalog.new_state();

        let error = catalog
            .set_groups_enabled(&state, [GIT, "unknown"], true)
            .expect_err("unknown group must reject the full batch");
        assert!(error.to_string().contains("unknown tool group 'unknown'"));
        assert!(!catalog.is_group_enabled(&state, GIT));

        let first = catalog
            .set_groups_enabled(&state, [GIT], true)
            .expect("enable git");
        assert_eq!(first.changed_groups, vec![GIT.to_string()]);
        assert!(first.unchanged_groups.is_empty());

        let second = catalog
            .set_groups_enabled(&state, [GIT], true)
            .expect("enable git again");
        assert!(second.changed_groups.is_empty());
        assert_eq!(second.unchanged_groups, vec![GIT.to_string()]);
        assert!(second.tools_added.is_empty());
        assert!(second.tools_removed.is_empty());
    }
}
