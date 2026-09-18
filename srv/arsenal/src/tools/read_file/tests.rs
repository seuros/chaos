use super::*;

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
