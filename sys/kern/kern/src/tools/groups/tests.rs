use super::*;

#[test]
fn catalog_is_strict_and_defaults_operational_groups_off() {
    let catalog = build_catalog().expect("tool group catalog");
    let state = new_state(&catalog, false).expect("tool group state");

    assert!(catalog.is_tool_visible(&state, "enable_tools"));
    assert!(!catalog.is_tool_visible(&state, "cron_create"));
    assert!(!catalog.is_tool_visible(&state, "read_file"));

    catalog
        .set_groups_enabled(&state, [CRON, FILESYSTEM], true)
        .expect("enable groups");
    assert!(catalog.is_tool_visible(&state, "cron_create"));
    assert!(catalog.is_tool_visible(&state, "cron_toggle"));
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
fn groups_are_independent() {
    let catalog = build_catalog().expect("tool group catalog");
    let state = catalog.new_state();

    catalog
        .set_groups_enabled(&state, [CRON], true)
        .expect("enable cron");
    assert!(catalog.is_tool_visible(&state, "cron_create"));
    assert!(!catalog.is_tool_visible(&state, "web_search"));

    catalog
        .set_groups_enabled(&state, [WEB], true)
        .expect("enable web");
    assert!(catalog.is_tool_visible(&state, "web_search"));
}

#[test]
fn states_do_not_inherit_activation() {
    let catalog = build_catalog().expect("tool group catalog");
    let parent = catalog.new_state();
    let child = catalog.new_state();
    catalog
        .set_groups_enabled(&parent, [CRON], true)
        .expect("enable cron");

    assert!(catalog.is_tool_visible(&parent, "cron_create"));
    assert!(!catalog.is_tool_visible(&child, "cron_create"));
}

#[test]
fn activation_is_atomic_and_idempotent_for_chaos_groups() {
    let catalog = build_catalog().expect("tool group catalog");
    let state = catalog.new_state();

    let error = catalog
        .set_groups_enabled(&state, [CRON, "unknown"], true)
        .expect_err("unknown group must reject the full batch");
    assert!(error.to_string().contains("unknown tool group 'unknown'"));
    assert!(!catalog.is_group_enabled(&state, CRON));

    let first = catalog
        .set_groups_enabled(&state, [CRON], true)
        .expect("enable cron");
    assert_eq!(first.changed_groups, vec![CRON.to_string()]);
    assert!(first.unchanged_groups.is_empty());

    let second = catalog
        .set_groups_enabled(&state, [CRON], true)
        .expect("enable cron again");
    assert!(second.changed_groups.is_empty());
    assert_eq!(second.unchanged_groups, vec![CRON.to_string()]);
    assert!(second.tools_added.is_empty());
    assert!(second.tools_removed.is_empty());
}
