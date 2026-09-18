use super::*;
use pretty_assertions::assert_eq;
use wiremock::http::HeaderMap;
use wiremock::http::Method;

fn request_with_input(input: Value) -> ResponsesRequest {
    ResponsesRequest(wiremock::Request {
        url: "http://localhost/v1/responses"
            .parse()
            .expect("valid request url"),
        method: Method::POST,
        headers: HeaderMap::new(),
        body: serde_json::to_vec(&serde_json::json!({ "input": input }))
            .expect("serialize request body"),
    })
}

#[test]
fn call_output_content_and_success_returns_only_single_text_content_item() {
    let single_text = request_with_input(serde_json::json!([
        {
            "type": "function_call_output",
            "call_id": "call-1",
            "output": [{ "type": "input_text", "text": "hello" }]
        },
        {
            "type": "custom_tool_call_output",
            "call_id": "call-2",
            "output": [{ "type": "input_text", "text": "world" }]
        }
    ]));
    assert_eq!(
        single_text.function_call_output_content_and_success("call-1"),
        Some((Some("hello".to_string()), None))
    );
    assert_eq!(
        single_text.custom_tool_call_output_content_and_success("call-2"),
        Some((Some("world".to_string()), None))
    );

    let mixed_content = request_with_input(serde_json::json!([
        {
            "type": "function_call_output",
            "call_id": "call-3",
            "output": [
                { "type": "input_text", "text": "hello" },
                { "type": "input_image", "image_url": "data:image/png;base64,abc" }
            ]
        },
        {
            "type": "custom_tool_call_output",
            "call_id": "call-4",
            "output": [{ "type": "input_image", "image_url": "data:image/png;base64,abc" }]
        }
    ]));
    assert_eq!(
        mixed_content.function_call_output_content_and_success("call-3"),
        Some((None, None))
    );
    assert_eq!(
        mixed_content.custom_tool_call_output_content_and_success("call-4"),
        Some((None, None))
    );
}
