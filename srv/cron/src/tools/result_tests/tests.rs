use super::*;
use mcp_host::prelude::{ToolError, ToolOutput};
use serde_json::json;

#[test]
fn structured_result_preserves_object_output() {
    let value = json!({"status": "created", "id": "job-1"});
    let expected = ToolOutput::structured(value.clone()).unwrap();
    let actual = structured_tool_result(Ok(value)).unwrap();
    assert_eq!(actual.into_response_value(), expected.into_response_value());
}

#[test]
fn structured_result_preserves_execution_error() {
    let message = "failed to look up job: unavailable".to_owned();
    match structured_tool_result(Err(message.clone())) {
        Err(ToolError::Execution(actual)) => assert_eq!(actual, message),
        other => panic!("expected execution error, got {other:?}"),
    }
}

#[test]
fn structured_result_preserves_non_object_error() {
    for value in [json!(null), json!([]), json!("text"), json!(42)] {
        let expected = ToolOutput::structured(value.clone()).unwrap_err();
        match structured_tool_result(Ok(value)) {
            Err(ToolError::Execution(actual)) => {
                assert_eq!(actual, format!("non-object tool output: {expected}"));
            }
            other => panic!("expected execution error, got {other:?}"),
        }
    }
}
