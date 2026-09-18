use super::*;
use pretty_assertions::assert_eq;

#[test]
fn verify_chaos_tool_json_schema() {
    let tool = chaos_tool_info();
    let tool_json = serde_json::to_value(&tool).expect("tool serializes");
    // Verify required fields exist.
    let input = tool_json.get("inputSchema").expect("inputSchema");
    let props = input.get("properties").expect("properties");
    assert!(props.get("prompt").is_some(), "prompt field required");
    assert!(
        props.get("process-id").is_some(),
        "process-id field required"
    );
    assert_eq!(tool_json.get("name"), Some(&json!("chaos")));
    assert_eq!(
        tool_json.get("execution"),
        Some(&json!({ "taskSupport": "optional" }))
    );
}
