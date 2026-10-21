use super::*;

fn control(call_id: &str, name: &str, groups: &[&str]) -> RolloutItem {
    RolloutItem::ResponseItem(
        serde_json::from_value(serde_json::json!({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": serde_json::json!({"groups": groups}).to_string(),
        }))
        .expect("control call"),
    )
}

fn acknowledged(call_id: &str, groups: &[&str]) -> RolloutItem {
    RolloutItem::ResponseItem(
        serde_json::from_value(serde_json::json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": serde_json::json!({
                "changed_groups": groups,
                "unchanged_groups": [],
                "tools_added": [],
                "tools_removed": [],
            }).to_string(),
        }))
        .expect("acknowledged control"),
    )
}

#[test]
fn resume_restores_acknowledged_controls_across_compaction_in_order() {
    let catalog = build_catalog().expect("catalog");
    let state = new_state(&catalog, false).expect("state");
    let history = vec![
        control("enable", "enable_tools", &[WEB, SHELL]),
        acknowledged("enable", &[WEB, SHELL]),
        RolloutItem::Compacted(chaos_ipc::protocol::CompactedItem {
            message: "compacted".into(),
            replacement_history: Vec::new(),
        }),
        control("disable", "disable_tools", &[SHELL]),
        acknowledged("disable", &[SHELL]),
    ];

    restore_from_rollout(&catalog, &state, &history);

    assert!(catalog.is_tool_visible(&state, "web_search"));
    assert!(!catalog.is_tool_visible(&state, "exec_command"));
    assert!(!catalog.is_tool_visible(&state, "apply_patch"));
}

#[test]
fn resume_ignores_incomplete_failed_malformed_and_unrelated_controls() {
    let mut failed = acknowledged("call", &[WEB]);
    if let RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { output, .. }) = &mut failed
    {
        output.success = Some(false);
    }
    let malformed = RolloutItem::ResponseItem(
        serde_json::from_value(serde_json::json!({
            "type": "function_call_output",
            "call_id": "call",
            "output": "unknown tool group",
        }))
        .expect("failed control output"),
    );
    let mut namespaced = control("call", "enable_tools", &[WEB]);
    if let RolloutItem::ResponseItem(ResponseItem::FunctionCall { namespace, .. }) = &mut namespaced
    {
        *namespace = Some("unrelated_provider".into());
    }
    for history in [
        vec![control("call", "enable_tools", &[WEB])],
        vec![acknowledged("call", &[WEB])],
        vec![control("call", "enable_tools", &[WEB]), failed],
        vec![control("call", "enable_tools", &[WEB]), malformed],
        vec![
            control("call", "enable_tools", &[WEB]),
            acknowledged("different_call", &[WEB]),
        ],
        vec![
            control("call", "enable_tools", &[WEB]),
            acknowledged("call", &[SHELL]),
        ],
        vec![
            control("call", "unrelated_tool", &[WEB]),
            acknowledged("call", &[WEB]),
        ],
        vec![namespaced, acknowledged("call", &[WEB])],
    ] {
        let catalog = build_catalog().expect("catalog");
        let state = new_state(&catalog, false).expect("state");
        restore_from_rollout(&catalog, &state, &history);
        assert!(!catalog.is_group_enabled(&state, WEB), "{history:?}");
    }
}

#[test]
fn resume_restores_unchanged_groups_and_ignores_retired_groups() {
    let catalog = build_catalog().expect("catalog");
    let state = new_state(&catalog, false).expect("state");
    let mut output = acknowledged("call", &[]);
    if let RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { output, .. }) = &mut output
    {
        output.body = FunctionCallOutputBody::Text(
            serde_json::json!({
                "changed_groups": ["retired_group"],
                "unchanged_groups": [WEB],
                "tools_added": [],
                "tools_removed": [],
            })
            .to_string(),
        );
    }
    restore_from_rollout(
        &catalog,
        &state,
        &[
            control("call", "enable_tools", &[WEB, "retired_group"]),
            output,
        ],
    );
    assert!(catalog.is_tool_visible(&state, "web_search"));
    assert!(!catalog.is_tool_visible(&state, "exec_command"));
}

#[test]
fn resume_does_not_apply_an_acknowledgment_twice() {
    let catalog = build_catalog().expect("catalog");
    let state = new_state(&catalog, false).expect("state");
    restore_from_rollout(
        &catalog,
        &state,
        &[
            control("enable", "enable_tools", &[WEB]),
            acknowledged("enable", &[WEB]),
            control("disable", "disable_tools", &[WEB]),
            acknowledged("disable", &[WEB]),
            acknowledged("enable", &[WEB]),
        ],
    );
    assert!(!catalog.is_tool_visible(&state, "web_search"));
}

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
