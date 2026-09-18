use super::ContextSnapshotOptions;
use super::ContextSnapshotRenderMode;
use super::format_response_items_snapshot;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn full_text_mode_normalizes_crlf_line_endings() {
    let items = vec![json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": "line one\r\n\r\nline two"
        }]
    })];

    let rendered = format_response_items_snapshot(
        &items,
        &ContextSnapshotOptions::default().render_mode(ContextSnapshotRenderMode::FullText),
    );

    assert_eq!(rendered, r"00:message/user:line one\n\nline two");
}

#[test]
fn redacted_text_mode_keeps_capability_instruction_placeholders() {
    let items = vec![json!({
        "type": "message",
        "role": "developer",
        "content": [
            {
                "type": "input_text",
                "text": "<apps_instructions>\n## Apps\nbody\n</apps_instructions>"
            },
            {
                "type": "input_text",
                "text": "<plugins_instructions>\n## Plugins\nbody\n</plugins_instructions>"
            }
        ]
    })];

    let rendered = format_response_items_snapshot(
        &items,
        &ContextSnapshotOptions::default().render_mode(ContextSnapshotRenderMode::RedactedText),
    );

    assert_eq!(
        rendered,
        "00:message/developer[2]:\n    [01] <APPS_INSTRUCTIONS>\n    [02] <PLUGINS_INSTRUCTIONS>"
    );
}

#[test]
fn strip_capability_instructions_omits_capability_parts_from_developer_messages() {
    let items = vec![json!({
        "type": "message",
        "role": "developer",
        "content": [
            { "type": "input_text", "text": "<permissions instructions>\n...</permissions instructions>" },
            { "type": "input_text", "text": "<plugins_instructions>\n## Plugins\n...</plugins_instructions>" }
        ]
    })];

    let rendered = format_response_items_snapshot(
        &items,
        &ContextSnapshotOptions::default()
            .render_mode(ContextSnapshotRenderMode::RedactedText)
            .strip_capability_instructions(),
    );

    assert_eq!(rendered, "00:message/developer:<PERMISSIONS_INSTRUCTIONS>");
}

#[test]
fn redacted_text_mode_normalizes_environment_context_with_subagents() {
    let items = vec![json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": "<environment_context>\n  <cwd>/tmp/example</cwd>\n  <shell>bash</shell>\n  <subagents>\n    - agent-1: atlas\n    - agent-2\n  </subagents>\n</environment_context>"
        }]
    })];

    let rendered = format_response_items_snapshot(
        &items,
        &ContextSnapshotOptions::default().render_mode(ContextSnapshotRenderMode::RedactedText),
    );

    assert_eq!(
        rendered,
        "00:message/user:<ENVIRONMENT_CONTEXT:cwd=<CWD>:subagents=2>"
    );
}

#[test]
fn kind_with_text_prefix_mode_normalizes_crlf_line_endings() {
    let items = vec![json!({
        "type": "message",
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": "<realtime_conversation>\r\nRealtime conversation started.\r\n\r\nYou are..."
        }]
    })];

    let rendered = format_response_items_snapshot(
        &items,
        &ContextSnapshotOptions::default()
            .render_mode(ContextSnapshotRenderMode::KindWithTextPrefix { max_chars: 64 }),
    );

    assert_eq!(
        rendered,
        r"00:message/developer:<realtime_conversation>\nRealtime conversation started.\n\nYou a..."
    );
}

#[test]
fn image_only_message_is_rendered_as_non_text_span() {
    let items = vec![json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_image",
            "image_url": "data:image/png;base64,AAAA"
        }]
    })];

    let rendered = format_response_items_snapshot(&items, &ContextSnapshotOptions::default());

    assert_eq!(rendered, "00:message/user:<input_image:image_url>");
}

#[test]
fn mixed_text_and_image_message_keeps_image_span() {
    let items = vec![json!({
        "type": "message",
        "role": "user",
        "content": [
            {
                "type": "input_text",
                "text": "<image>"
            },
            {
                "type": "input_image",
                "image_url": "data:image/png;base64,AAAA"
            },
            {
                "type": "input_text",
                "text": "</image>"
            }
        ]
    })];

    let rendered = format_response_items_snapshot(&items, &ContextSnapshotOptions::default());

    assert_eq!(
        rendered,
        "00:message/user[3]:\n    [01] <image>\n    [02] <input_image:image_url>\n    [03] </image>"
    );
}

#[test]
fn redacted_text_mode_normalizes_system_skill_temp_paths() {
    let items = vec![json!({
        "type": "message",
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": "## Skills\n- openai-docs: helper (file: /private/var/folders/yk/p4jp9nzs79s5q84csslkgqtm0000gn/T/.tmpAnGVww/skills/.system/openai-docs/SKILL.md)"
        }]
    })];

    let rendered = format_response_items_snapshot(&items, &ContextSnapshotOptions::default());

    assert_eq!(
        rendered,
        "00:message/developer:## Skills\\n- openai-docs: helper (file: <SYSTEM_SKILLS_ROOT>/openai-docs/SKILL.md)"
    );
}
