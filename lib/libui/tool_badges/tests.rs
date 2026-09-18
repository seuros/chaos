use super::*;
pub(crate) fn tool_badges_suite() {
    merged_annotations_upgrade_closed_world_tool_to_writes();
    snake_case_annotations_also_render_writes();
}

fn merged_annotations_upgrade_closed_world_tool_to_writes() {
    let style = tool_name_style_from_labels_and_annotations(
        &["closed-world".to_string()],
        Some(&serde_json::json!({
            "readOnlyHint": false,
            "openWorldHint": false
        })),
    );

    assert_eq!(style.fg, Some(crate::theme::warning_color()));
}

fn snake_case_annotations_also_render_writes() {
    let style = tool_name_style_from_annotations(Some(&serde_json::json!({
        "read_only_hint": false,
        "open_world_hint": false
    })));

    assert_eq!(style.fg, Some(crate::theme::warning_color()));
}
