use super::super::*;
use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::TurnRequest;
use chaos_test_fixtures::TEST_MODEL;
use serde_json::Value;
#[test]
fn output_cap_is_opt_in_for_streaming_and_structured_chat() {
    let mut request = TurnRequest {
        model: "model".into(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };
    assert_eq!(
        build_request_body(&request, "model", true).unwrap(),
        serde_json::json!({
            "model": "model",
            "stream": true,
            "stream_options": {"include_usage": true}
        })
    );
    for stream in [true, false] {
        request.extensions.clear();
        request.output_schema = (!stream).then(|| serde_json::json!({"type": "object"}));
        let baseline = build_request_body(&request, "model", stream).unwrap();
        for cap in [1, 37, u32::MAX] {
            request
                .extensions
                .insert("max_output_tokens".into(), serde_json::json!(cap));
            let mut body = build_request_body(&request, "model", stream).unwrap();
            assert_eq!(body["max_completion_tokens"], cap);
            assert!(body.get("max_output_tokens").is_none());
            assert!(body.get("max_tokens").is_none());
            body.as_object_mut()
                .unwrap()
                .remove("max_completion_tokens");
            assert_eq!(
                serde_json::to_vec(&body).unwrap(),
                serde_json::to_vec(&baseline).unwrap()
            );
        }
    }
}

#[test]
fn json_headers_preserve_custom_headers_and_replace_accept() {
    let mut adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
        "http://localhost".into(),
        "secret".into(),
        None,
    );
    adapter
        .provider
        .headers
        .insert("x-custom", "value".parse().unwrap());
    adapter
        .provider
        .headers
        .insert("accept", "text/event-stream".parse().unwrap());
    let headers = adapter.build_headers(false).unwrap();
    assert_eq!(headers["authorization"], "Bearer secret");
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(headers["accept"], "application/json");
    assert_eq!(headers["x-custom"], "value");
}

#[test]
fn build_request_body_preserves_output_schema() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"verdict": {"type": "string"}},
        "required": ["verdict"],
        "additionalProperties": false
    });
    let mut request = TurnRequest {
        model: "test-model".to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: Some(schema.clone()),
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };

    let body = build_request_body(&request, "test-model", false).expect("body should build");
    assert_eq!(body["stream"], false);
    assert!(body.get("stream_options").is_none());
    assert_eq!(
        body["response_format"],
        serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "codex_output_schema",
                "strict": true,
                "schema": schema
            }
        })
    );
    request.output_schema = None;
    let body = build_request_body(&request, "test-model", true).expect("body should build");
    assert!(body.get("response_format").is_none());
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[test]
fn build_request_body_merges_every_system_message_into_the_leading_block() {
    let message = |role: &str, text: &str| ResponseItem::Message {
        id: None,
        role: role.into(),
        content: vec![ContentItem::InputText { text: text.into() }],
        end_turn: None,
        phase: None,
    };
    let request = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: "base".to_string(),
        input: vec![
            message("developer", "permissions"),
            message("user", "hello"),
            message("system", "tools reminder"),
        ],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };
    let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
    let messages = body["messages"].as_array().expect("messages");
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(roles, vec!["system", "user"]);
    assert_eq!(
        messages[0]["content"],
        "base\n\npermissions\n\ntools reminder"
    );
    assert_eq!(messages[1]["content"], "hello");
}

#[test]
fn build_request_body_hoists_a_lone_later_system_message_without_instructions() {
    let message = |role: &str, text: &str| ResponseItem::Message {
        id: None,
        role: role.into(),
        content: vec![ContentItem::InputText { text: text.into() }],
        end_turn: None,
        phase: None,
    };
    let request = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: String::new(),
        input: vec![
            message("user", "hello"),
            message("system", "tools reminder"),
        ],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };
    let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
    let messages = body["messages"].as_array().expect("messages");
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(roles, vec!["system", "user"]);
    assert_eq!(messages[0]["content"], "tools reminder");
    assert_eq!(messages[1]["content"], "hello");
}

#[test]
fn build_request_body_includes_system_message() {
    let request = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: "You are helpful".to_string(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };

    let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
    let messages = body.get("messages").and_then(Value::as_array).unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are helpful");
}

#[test]
fn build_request_body_no_system_when_no_instructions() {
    let request = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };

    let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
    let messages = body.get("messages").and_then(Value::as_array);
    assert!(
        messages.is_none(),
        "expected messages to be omitted when empty"
    );
}

#[test]
fn build_headers_rejects_empty_api_key() {
    let adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
        "https://api.openai.com/v1".to_string(),
        String::new(),
        None,
    );
    assert!(adapter.build_headers(true).is_err());
    assert!(adapter.build_headers(false).is_err());
}

#[test]
fn build_headers_omits_authorization_without_api_key() {
    let provider = Provider::from_base_url_with_default_streaming_config(
        "ChatCompletions",
        "http://127.0.0.1:8000/v1".to_string(),
        false,
    );
    let adapter = ChatCompletionsAdapter::new(provider, None, None);
    for stream in [true, false] {
        let headers = adapter.build_headers(stream).expect("headers should build");
        assert!(headers.get(rama::http::header::AUTHORIZATION).is_none());
    }
}

#[test]
fn build_headers_includes_bearer_token() {
    let adapter = ChatCompletionsAdapter::from_base_url_and_api_key(
        "https://api.openai.com/v1".to_string(),
        "sk-test".to_string(),
        None,
    );
    let headers = adapter.build_headers(true).expect("headers should build");
    assert_eq!(
        headers
            .get(rama::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer sk-test")
    );
}

fn stop_hook_request(input: Vec<ResponseItem>) -> TurnRequest {
    TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: "base".to_string(),
        input,
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    }
}

fn text_message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.into(),
        content: vec![ContentItem::InputText { text: text.into() }],
        end_turn: None,
        phase: None,
    }
}

#[test]
fn stop_hook_continuation_stays_in_place_as_user_so_the_wire_never_ends_on_assistant() {
    let continuation: ResponseItem =
        chaos_ipc::models::DeveloperInstructions::stop_hook_continuation("write a journal entry")
            .into();
    let continuation_text = match &continuation {
        ResponseItem::Message { content, .. } => match &content[0] {
            ContentItem::InputText { text } => text.clone(),
            other => panic!("unexpected content {other:?}"),
        },
        other => panic!("unexpected item {other:?}"),
    };
    let request = stop_hook_request(vec![
        text_message("system", "permissions"),
        text_message("user", "hello"),
        text_message("assistant", "done"),
        continuation,
    ]);
    let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
    let messages = body["messages"].as_array().expect("messages");
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
    assert_eq!(messages[0]["content"], "base\n\npermissions");
    assert_eq!(messages[3]["content"], continuation_text.as_str());
    assert!(continuation_text.contains("write a journal entry"));
}

#[test]
fn only_an_exact_stop_hook_block_is_kept_as_user() {
    let open = chaos_ipc::protocol::STOP_HOOK_CONTINUATION_OPEN_TAG;
    let close = chaos_ipc::protocol::STOP_HOOK_CONTINUATION_CLOSE_TAG;
    let quoted = format!("operator note quoting {open}\nx\n{close}");
    let two_blocks = format!("{open}\na\n{close}{open}\nb\n{close}");
    let request = stop_hook_request(vec![
        text_message("user", "hello"),
        text_message("assistant", "done"),
        text_message("developer", &quoted),
        text_message("system", &two_blocks),
    ]);
    let body = build_request_body(&request, TEST_MODEL, true).expect("body should build");
    let messages = body["messages"].as_array().expect("messages");
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(roles, vec!["system", "user", "assistant"]);
    assert_eq!(
        messages[0]["content"],
        format!("base\n\n{quoted}\n\n{two_blocks}").as_str()
    );
}
