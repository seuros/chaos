use super::*;

#[test]
fn referenced_step_union_exposes_all_discriminators_on_the_wire() {
    let steps = [
        ("pr_opened", "head_prefix"),
        ("pr_merged", "pr"),
        ("run_finished", "workflow"),
    ];
    let variants = steps.map(|(tag, field)| json!({
        "type": "object",
        "properties": {"wait_for": {"type": "string", "const": tag}, (field): {"type": "string"}},
        "required": ["wait_for"]
    }));
    let tool = mcp_tool_to_responses_api_tool(
        "watch_until".into(),
        None,
        json!({"type": "object", "properties": {
            "steps": {"type": "array", "items": {"$ref": "#/$defs/Step"}}
        }, "$defs": {"Step": {"oneOf": variants}}}),
        None,
        false,
    )
    .unwrap();
    let wire = serde_json::to_value(tool).unwrap();
    let item = &wire["parameters"]["properties"]["steps"]["items"];
    assert_eq!(item["required"], json!(["wait_for"]));
    assert_eq!(
        item["properties"]["wait_for"],
        json!({
            "type": "string", "enum": ["pr_opened", "pr_merged", "run_finished"]
        })
    );
    for (_, field) in steps {
        assert_eq!(item["properties"][field], json!({"type": "string"}));
    }
}

#[test]
fn open_string_branch_is_not_narrowed_to_the_other_choices() {
    let schema = parse_tool_input_schema(&json!({"anyOf": [
        {"type": "string", "enum": ["known"]}, {"type": "string"}
    ]}))
    .unwrap();
    assert_eq!(JsonValue::from(&schema), json!({"type": "string"}));
}
