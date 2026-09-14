#[test]
fn mutation_tools_are_marked_mutating_and_non_parallel() {
    let tools = super::git_catalog_tools();
    for name in ["git_add", "git_commit", "git_branch"] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(tool.read_only_hint, Some(false));
        assert!(!tool.supports_parallel_tool_calls);
    }

    let status = tools
        .iter()
        .find(|tool| tool.name == "git_status")
        .expect("git_status");
    assert_eq!(status.read_only_hint, Some(true));
    assert!(status.supports_parallel_tool_calls);
    assert!(
        tools.iter().all(|tool| tool.name != "git_branches"),
        "branch listing must be exposed as a resource, not a tool"
    );
}

#[test]
fn git_diff_schema_requires_structured_scope_format_and_check() {
    let tools = super::git_catalog_tools();
    let diff = tools
        .iter()
        .find(|tool| tool.name == "git_diff")
        .expect("git_diff");
    let required = diff.input_schema["required"]
        .as_array()
        .expect("required properties");

    for name in ["scope", "format", "check"] {
        assert!(
            required.iter().any(|value| value == name),
            "{name} must be required: {}",
            diff.input_schema
        );
    }
    for name in ["base", "paths"] {
        assert!(
            required.iter().all(|value| value != name),
            "{name} must remain optional: {}",
            diff.input_schema
        );
    }
}

#[test]
fn git_commit_schema_exposes_optional_trailers_and_destructive_amend() {
    let commit = super::tools::tool_infos()
        .into_iter()
        .find(|tool| tool.name == "git_commit")
        .expect("git_commit");
    let required = commit.input_schema["required"]
        .as_array()
        .expect("required properties");

    assert!(required.iter().any(|value| value == "message"));
    assert!(required.iter().all(|value| value != "amend"));
    assert!(required.iter().all(|value| value != "trailers"));
    assert_eq!(
        commit.input_schema["properties"]["amend"]["type"],
        "boolean"
    );
    assert_eq!(
        commit.input_schema["properties"]["trailers"]["type"],
        "array"
    );
    let destructive = commit
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.destructive_hint)
        .unwrap_or(true);
    assert!(destructive);
}

#[test]
fn git_show_file_schema_requires_path_and_uses_integer_line_fields() {
    let tool = super::git_catalog_tools()
        .into_iter()
        .find(|tool| tool.name == "git_show_file")
        .expect("git_show_file");
    let required = tool.input_schema["required"]
        .as_array()
        .expect("required properties");

    assert!(required.iter().any(|value| value == "file_path"));
    for name in ["rev", "start_line", "end_line"] {
        assert!(
            required.iter().all(|value| value != name),
            "{name} must remain optional: {}",
            tool.input_schema
        );
    }
    for name in ["start_line", "end_line"] {
        let ty = &tool.input_schema["properties"][name]["type"];
        assert!(
            *ty == "integer" || *ty == serde_json::json!(["integer", "null"]),
            "{name} must be advertised as integer: {ty}"
        );
    }
    assert_eq!(tool.read_only_hint, Some(true));
    assert!(tool.supports_parallel_tool_calls);
}

#[test]
fn branch_resource_template_is_registered() {
    let templates = super::git_resource_templates();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].uri_template, "git://branches{?scope,contains}");
    assert_eq!(templates[0].mime_type.as_deref(), Some("application/json"));
}
