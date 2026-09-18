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
