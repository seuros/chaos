use super::*;

#[tokio::test]
async fn editor_and_disk_sources_share_exact_slice_and_indentation_results() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("format.rs");
    for text in ["fn main() {\r\n\t// header\r\n\tbody();\r\n}\r\n", "one\n\nthree", "🦀\n"] {
        tokio::fs::write(&file, text).await.unwrap();
        for mode in ["slice", "indentation"] {
            let args = serde_json::json!({
                "file_path":file,"offset":1,"limit":3,"mode":mode
            });
            assert_eq!(
                execute_structured(&args).await,
                execute_editor_structured(&args, text).await,
            );
        }
    }
}

#[tokio::test]
async fn indentation_capacity_is_bounded_by_available_lines_not_requested_limit() {
    let args = serde_json::json!({
        "file_path":"/existing-buffer.rs",
        "mode":"indentation",
        "limit":usize::MAX,
    });
    let output = execute_editor_structured(&args, "first\nsecond\n").await.unwrap();
    assert_eq!(output["line_count"], 2);
}

#[test]
fn schema_generates_valid_json() {
    let info = ChaosServer::read_file_tool_info();
    assert_eq!(info.name, "read_file");
    assert!(info.description.is_some());

    // Verify input_schema is a valid JSON object with properties
    let schema = &info.input_schema;
    assert!(schema.is_object(), "input_schema must be a JSON object");

    let obj = schema.as_object().unwrap();
    let props = obj.get("properties").unwrap().as_object().unwrap();
    assert!(
        props.contains_key("file_path"),
        "schema must have file_path property"
    );
    assert!(
        props.contains_key("offset"),
        "schema must have offset property"
    );
    assert!(
        props.contains_key("limit"),
        "schema must have limit property"
    );
    assert_eq!(props["offset"]["type"], "integer");
    assert_eq!(props["limit"]["type"], "integer");
    assert!(props.contains_key("mode"), "schema must have mode property");
    assert!(
        props.contains_key("indentation"),
        "schema must have indentation property"
    );

    // Verify required fields
    let required = obj.get("required").unwrap().as_array().unwrap();
    assert!(required.contains(&serde_json::Value::String("file_path".to_string())));
}

#[test]
fn router_contains_all_tools() {
    let router = crate::tools::router();
    let tools = router.list();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"grep_files"));
    assert!(names.contains(&"list_dir"));
    assert!(names.contains(&"locate_files"));
    assert_eq!(tools.len(), 4);
}

#[tokio::test]
async fn rejects_invalid_arguments() {
    for field in ["offset", "limit", "anchor_line", "max_levels", "max_lines"] {
        for value in [
            serde_json::json!(1.0),
            serde_json::json!(1.5),
            serde_json::json!(-1),
            serde_json::json!("1"),
            serde_json::json!(1e100),
        ] {
            let mut args = serde_json::json!({"file_path": "/tmp/example.rs"});
            if matches!(field, "offset" | "limit") {
                args[field] = value;
            } else {
                args["indentation"] = serde_json::json!({field: value});
            }
            let err = execute(&args).await.expect_err("invalid line argument");
            assert!(err.starts_with("invalid arguments:"), "{err}");
            assert!(err.contains("usize"), "{err}");
        }
    }
    let result = execute(&serde_json::json!({
        "file_path": "/tmp/example.rs",
        "indentation": {
            "anchor_line": 1,
            "unknwon_field": true
        }
    }))
    .await;

    let err = result.expect_err("unknown indentation field should fail");
    assert!(err.contains("unknown field `unknwon_field`"));
}
