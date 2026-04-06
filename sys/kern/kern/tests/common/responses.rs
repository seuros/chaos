use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ModelsResponse;
use serde_json::Value;
use wiremock::BodyPrintLimit;
use wiremock::Match;
use wiremock::Mock;
use wiremock::MockBuilder;
use wiremock::MockServer;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

use crate::test_codex::ApplyPatchModelOutput;

#[derive(Debug, Clone)]
pub struct ResponseMock {
    requests: Arc<Mutex<Vec<ResponsesRequest>>>,
}

impl ResponseMock {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn single_request(&self) -> ResponsesRequest {
        let requests = self.requests.lock().unwrap();
        if requests.len() != 1 {
            panic!("expected 1 request, got {}", requests.len());
        }
        requests.first().unwrap().clone()
    }

    pub fn requests(&self) -> Vec<ResponsesRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn last_request(&self) -> Option<ResponsesRequest> {
        self.requests.lock().unwrap().last().cloned()
    }

    /// Returns true if any captured request contains a `function_call` with the
    /// provided `call_id`.
    pub fn saw_function_call(&self, call_id: &str) -> bool {
        self.requests()
            .iter()
            .any(|req| req.has_function_call(call_id))
    }

    /// Returns the `output` string for a matching `function_call_output` with
    /// the provided `call_id`, searching across all captured requests.
    pub fn function_call_output_text(&self, call_id: &str) -> Option<String> {
        self.requests()
            .iter()
            .find_map(|req| req.function_call_output_text(call_id))
    }
}

#[derive(Debug, Clone)]
pub struct ResponsesRequest(wiremock::Request);

fn is_zstd_encoding(value: &str) -> bool {
    value
        .split(',')
        .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
}

fn decode_body_bytes(body: &[u8], content_encoding: Option<&str>) -> Vec<u8> {
    if content_encoding.is_some_and(is_zstd_encoding) {
        zstd::stream::decode_all(std::io::Cursor::new(body)).unwrap_or_else(|err| {
            panic!("failed to decode zstd request body: {err}");
        })
    } else {
        body.to_vec()
    }
}

impl ResponsesRequest {
    pub fn body_json(&self) -> Value {
        let body = decode_body_bytes(
            &self.0.body,
            self.0
                .headers
                .get("content-encoding")
                .and_then(|value| value.to_str().ok()),
        );
        serde_json::from_slice(&body).unwrap()
    }

    pub fn body_bytes(&self) -> Vec<u8> {
        self.0.body.clone()
    }

    pub fn body_contains_text(&self, text: &str) -> bool {
        let json_fragment = serde_json::to_string(text)
            .expect("serialize text to JSON")
            .trim_matches('"')
            .to_string();
        self.body_json().to_string().contains(&json_fragment)
    }

    pub fn instructions_text(&self) -> String {
        self.body_json()["instructions"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Returns all `input_text` spans from `message` inputs for the provided role.
    pub fn message_input_texts(&self, role: &str) -> Vec<String> {
        self.inputs_of_type("message")
            .into_iter()
            .filter(|item| item.get("role").and_then(Value::as_str) == Some(role))
            .filter_map(|item| item.get("content").and_then(Value::as_array).cloned())
            .flatten()
            .filter(|span| span.get("type").and_then(Value::as_str) == Some("input_text"))
            .filter_map(|span| span.get("text").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }

    /// Returns `input_text` spans grouped by `message` input for the provided role.
    pub fn message_input_text_groups(&self, role: &str) -> Vec<Vec<String>> {
        self.inputs_of_type("message")
            .into_iter()
            .filter(|item| item.get("role").and_then(Value::as_str) == Some(role))
            .filter_map(|item| item.get("content").and_then(Value::as_array).cloned())
            .map(|content| {
                content
                    .into_iter()
                    .filter(|span| span.get("type").and_then(Value::as_str) == Some("input_text"))
                    .filter_map(|span| span.get("text").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .collect()
    }

    pub fn has_message_with_input_texts(
        &self,
        role: &str,
        predicate: impl Fn(&[String]) -> bool,
    ) -> bool {
        self.message_input_text_groups(role)
            .iter()
            .any(|texts| predicate(texts))
    }

    /// Returns all `input_image` `image_url` spans from `message` inputs for the provided role.
    pub fn message_input_image_urls(&self, role: &str) -> Vec<String> {
        self.inputs_of_type("message")
            .into_iter()
            .filter(|item| item.get("role").and_then(Value::as_str) == Some(role))
            .filter_map(|item| item.get("content").and_then(Value::as_array).cloned())
            .flatten()
            .filter(|span| span.get("type").and_then(Value::as_str) == Some("input_image"))
            .filter_map(|span| {
                span.get("image_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect()
    }

    pub fn input(&self) -> Vec<Value> {
        self.body_json()["input"]
            .as_array()
            .expect("input array not found in request")
            .clone()
    }

    pub fn inputs_of_type(&self, ty: &str) -> Vec<Value> {
        self.input()
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some(ty))
            .cloned()
            .collect()
    }

    pub fn function_call_output(&self, call_id: &str) -> Value {
        self.call_output(call_id, "function_call_output")
    }

    pub fn custom_tool_call_output(&self, call_id: &str) -> Value {
        self.call_output(call_id, "custom_tool_call_output")
    }

    pub fn tool_search_output(&self, call_id: &str) -> Value {
        self.call_output(call_id, "tool_search_output")
    }

    pub fn call_output(&self, call_id: &str, call_type: &str) -> Value {
        self.input()
            .iter()
            .find(|item| {
                item.get("type").unwrap() == call_type && item.get("call_id").unwrap() == call_id
            })
            .cloned()
            .unwrap_or_else(|| panic!("function call output {call_id} item not found in request"))
    }

    /// Returns true if this request's `input` contains a `function_call` with
    /// the specified `call_id`.
    pub fn has_function_call(&self, call_id: &str) -> bool {
        self.input().iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
    }

    /// If present, returns the `output` string of the `function_call_output`
    /// entry matching `call_id` in this request's `input`.
    pub fn function_call_output_text(&self, call_id: &str) -> Option<String> {
        let binding = self.input();
        let item = binding.iter().find(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some(call_id)
        })?;
        item.get("output")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub fn function_call_output_content_and_success(
        &self,
        call_id: &str,
    ) -> Option<(Option<String>, Option<bool>)> {
        self.call_output_content_and_success(call_id, "function_call_output")
    }

    pub fn custom_tool_call_output_content_and_success(
        &self,
        call_id: &str,
    ) -> Option<(Option<String>, Option<bool>)> {
        self.call_output_content_and_success(call_id, "custom_tool_call_output")
    }

    fn call_output_content_and_success(
        &self,
        call_id: &str,
        call_type: &str,
    ) -> Option<(Option<String>, Option<bool>)> {
        let output = self
            .call_output(call_id, call_type)
            .get("output")
            .cloned()
            .unwrap_or(Value::Null);
        match output {
            Value::String(_) | Value::Array(_) => Some((output_value_to_text(&output), None)),
            Value::Object(obj) => Some((
                obj.get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                obj.get("success").and_then(Value::as_bool),
            )),
            _ => Some((None, None)),
        }
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.0
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    pub fn path(&self) -> String {
        self.0.url.path().to_string()
    }

    pub fn query_param(&self, name: &str) -> Option<String> {
        self.0
            .url
            .query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string())
    }
}

pub(crate) fn output_value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => match items.as_slice() {
            [item] if item.get("type").and_then(Value::as_str) == Some("input_text") => {
                item.get("text").and_then(Value::as_str).map(str::to_string)
            }
            [_] | [] | [_, _, ..] => None,
        },
        Value::Object(_) | Value::Number(_) | Value::Bool(_) | Value::Null => None,
    }
}

#[cfg(test)]
mod tests {
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
}


#[derive(Debug, Clone)]
pub struct ModelsMock {
    requests: Arc<Mutex<Vec<wiremock::Request>>>,
}

impl ModelsMock {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn requests(&self) -> Vec<wiremock::Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn single_request_path(&self) -> String {
        let requests = self.requests.lock().unwrap();
        if requests.len() != 1 {
            panic!("expected 1 request, got {}", requests.len());
        }
        requests.first().unwrap().url.path().to_string()
    }
}

impl Match for ModelsMock {
    fn matches(&self, request: &wiremock::Request) -> bool {
        self.requests.lock().unwrap().push(request.clone());
        true
    }
}

impl Match for ResponseMock {
    fn matches(&self, request: &wiremock::Request) -> bool {
        self.requests
            .lock()
            .unwrap()
            .push(ResponsesRequest(request.clone()));

        // Enforce invariant checks on every request body captured by the mock.
        // Panic on orphan tool outputs or calls to catch regressions early.
        validate_request_body_invariants(request);
        true
    }
}

/// Build an SSE stream body from a list of JSON events.
pub fn sse(events: Vec<Value>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for ev in events {
        let kind = ev.get("type").and_then(|v| v.as_str()).unwrap();
        writeln!(&mut out, "event: {kind}").unwrap();
        if !ev.as_object().map(|o| o.len() == 1).unwrap_or(false) {
            write!(&mut out, "data: {ev}\n\n").unwrap();
        } else {
            out.push('\n');
        }
    }
    out
}

pub fn sse_completed(id: &str) -> String {
    sse(vec![ev_response_created(id), ev_completed(id)])
}

/// Convenience: SSE event for a completed response with a specific id.
pub fn ev_completed(id: &str) -> Value {
    serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": id,
            "usage": {"input_tokens":0,"input_tokens_details":null,"output_tokens":0,"output_tokens_details":null,"total_tokens":0}
        }
    })
}

/// Convenience: SSE event for a created response with a specific id.
pub fn ev_response_created(id: &str) -> Value {
    serde_json::json!({
        "type": "response.created",
        "response": {
            "id": id,
        }
    })
}

pub fn ev_completed_with_tokens(id: &str, total_tokens: i64) -> Value {
    serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": id,
            "usage": {
                "input_tokens": total_tokens,
                "input_tokens_details": null,
                "output_tokens": 0,
                "output_tokens_details": null,
                "total_tokens": total_tokens
            }
        }
    })
}

/// Convenience: SSE event for a single assistant message output item.
pub fn ev_assistant_message(id: &str, text: &str) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "id": id,
            "content": [{"type": "output_text", "text": text}]
        }
    })
}

pub fn user_message_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

pub fn ev_message_item_added(id: &str, text: &str) -> Value {
    serde_json::json!({
        "type": "response.output_item.added",
        "item": {
            "type": "message",
            "role": "assistant",
            "id": id,
            "content": [{"type": "output_text", "text": text}]
        }
    })
}

pub fn ev_output_text_delta(delta: &str) -> Value {
    serde_json::json!({
        "type": "response.output_text.delta",
        "delta": delta,
    })
}

pub fn ev_reasoning_item(id: &str, summary: &[&str], raw_content: &[&str]) -> Value {
    let summary_entries: Vec<Value> = summary
        .iter()
        .map(|text| serde_json::json!({"type": "summary_text", "text": text}))
        .collect();

    let overhead = "b".repeat(550);
    let raw_content_joined = raw_content.join("");
    let encrypted_content =
        base64::engine::general_purpose::STANDARD.encode(overhead + raw_content_joined.as_str());

    let mut event = serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "reasoning",
            "id": id,
            "summary": summary_entries,
            "encrypted_content": encrypted_content,
        }
    });

    if !raw_content.is_empty() {
        let content_entries: Vec<Value> = raw_content
            .iter()
            .map(|text| serde_json::json!({"type": "reasoning_text", "text": text}))
            .collect();
        event["item"]["content"] = Value::Array(content_entries);
    }

    event
}

pub fn ev_reasoning_item_added(id: &str, summary: &[&str]) -> Value {
    let summary_entries: Vec<Value> = summary
        .iter()
        .map(|text| serde_json::json!({"type": "summary_text", "text": text}))
        .collect();

    serde_json::json!({
        "type": "response.output_item.added",
        "item": {
            "type": "reasoning",
            "id": id,
            "summary": summary_entries,
        }
    })
}

pub fn ev_reasoning_summary_text_delta(delta: &str) -> Value {
    serde_json::json!({
        "type": "response.reasoning_summary_text.delta",
        "delta": delta,
        "summary_index": 0,
    })
}

pub fn ev_reasoning_text_delta(delta: &str) -> Value {
    serde_json::json!({
        "type": "response.reasoning_text.delta",
        "delta": delta,
        "content_index": 0,
    })
}

pub fn ev_web_search_call_added_partial(id: &str, status: &str) -> Value {
    serde_json::json!({
        "type": "response.output_item.added",
        "item": {
            "type": "web_search_call",
            "id": id,
            "status": status
        }
    })
}

pub fn ev_web_search_call_done(id: &str, status: &str, query: &str) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "web_search_call",
            "id": id,
            "status": status,
            "action": {"type": "search", "query": query}
        }
    })
}

pub fn ev_image_generation_call(
    id: &str,
    status: &str,
    revised_prompt: &str,
    result: &str,
) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "image_generation_call",
            "id": id,
            "status": status,
            "revised_prompt": revised_prompt,
            "result": result,
        }
    })
}

pub fn ev_function_call(call_id: &str, name: &str, arguments: &str) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": arguments
        }
    })
}

pub fn ev_tool_search_call(call_id: &str, arguments: &serde_json::Value) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "tool_search_call",
            "call_id": call_id,
            "execution": "client",
            "arguments": arguments,
        }
    })
}

pub fn ev_custom_tool_call(call_id: &str, name: &str, input: &str) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "custom_tool_call",
            "call_id": call_id,
            "name": name,
            "input": input
        }
    })
}

pub fn ev_local_shell_call(call_id: &str, status: &str, command: Vec<&str>) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "local_shell_call",
            "call_id": call_id,
            "status": status,
            "action": {
                "type": "exec",
                "command": command,
            }
        }
    })
}

pub fn ev_apply_patch_call(
    call_id: &str,
    patch: &str,
    output_type: ApplyPatchModelOutput,
) -> Value {
    match output_type {
        ApplyPatchModelOutput::Freeform => ev_apply_patch_custom_tool_call(call_id, patch),
        ApplyPatchModelOutput::Function => ev_apply_patch_function_call(call_id, patch),
        ApplyPatchModelOutput::Shell => ev_apply_patch_shell_call(call_id, patch),
        ApplyPatchModelOutput::ShellViaHeredoc => {
            ev_apply_patch_shell_call_via_heredoc(call_id, patch)
        }
        ApplyPatchModelOutput::ShellCommandViaHeredoc => {
            ev_apply_patch_shell_command_call_via_heredoc(call_id, patch)
        }
    }
}

/// Convenience: SSE event for an `apply_patch` custom tool call with raw patch
/// text. This mirrors the payload produced by the Responses API when the model
/// invokes `apply_patch` directly (before we convert it to a function call).
pub fn ev_apply_patch_custom_tool_call(call_id: &str, patch: &str) -> Value {
    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "custom_tool_call",
            "name": "apply_patch",
            "input": patch,
            "call_id": call_id
        }
    })
}

/// Convenience: SSE event for an `apply_patch` function call. The Responses API
/// wraps the patch content in a JSON string under the `input` key; we recreate
/// the same structure so downstream code exercises the full parsing path.
pub fn ev_apply_patch_function_call(call_id: &str, patch: &str) -> Value {
    let arguments = serde_json::json!({ "input": patch });
    let arguments = serde_json::to_string(&arguments).expect("serialize apply_patch arguments");

    serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "name": "apply_patch",
            "arguments": arguments,
            "call_id": call_id
        }
    })
}

pub fn ev_shell_command_call(call_id: &str, command: &str) -> Value {
    let args = serde_json::json!({ "command": command });
    ev_shell_command_call_with_args(call_id, &args)
}

pub fn ev_shell_command_call_with_args(call_id: &str, args: &serde_json::Value) -> Value {
    let arguments = serde_json::to_string(args).expect("serialize shell command arguments");
    ev_function_call(call_id, "shell_command", &arguments)
}

pub fn ev_apply_patch_shell_call(call_id: &str, patch: &str) -> Value {
    let args = serde_json::json!({ "command": ["apply_patch", patch] });
    let arguments = serde_json::to_string(&args).expect("serialize apply_patch arguments");

    ev_function_call(call_id, "shell", &arguments)
}

pub fn ev_apply_patch_shell_call_via_heredoc(call_id: &str, patch: &str) -> Value {
    let script = format!("apply_patch <<'EOF'\n{patch}\nEOF\n");
    let args = serde_json::json!({ "command": ["bash", "-lc", script] });
    let arguments = serde_json::to_string(&args).expect("serialize apply_patch arguments");

    ev_function_call(call_id, "shell", &arguments)
}

pub fn ev_apply_patch_shell_command_call_via_heredoc(call_id: &str, patch: &str) -> Value {
    let args = serde_json::json!({ "command": format!("apply_patch <<'EOF'\n{patch}\nEOF\n") });
    let arguments = serde_json::to_string(&args).expect("serialize apply_patch arguments");

    ev_function_call(call_id, "shell_command", &arguments)
}

pub fn sse_failed(id: &str, code: &str, message: &str) -> String {
    sse(vec![serde_json::json!({
        "type": "response.failed",
        "response": {
            "id": id,
            "error": {"code": code, "message": message}
        }
    })])
}

pub fn sse_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(body, "text/event-stream")
}

pub async fn mount_response_once(server: &MockServer, response: ResponseTemplate) -> ResponseMock {
    let (mock, response_mock) = base_mock();
    mock.respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
    response_mock
}

pub async fn mount_response_once_match<M>(
    server: &MockServer,
    matcher: M,
    response: ResponseTemplate,
) -> ResponseMock
where
    M: wiremock::Match + Send + Sync + 'static,
{
    let (mock, response_mock) = base_mock();
    mock.and(matcher)
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
    response_mock
}

fn base_mock() -> (MockBuilder, ResponseMock) {
    let response_mock = ResponseMock::new();
    let mock = Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .and(response_mock.clone());
    (mock, response_mock)
}

fn compact_mock() -> (MockBuilder, ResponseMock) {
    let response_mock = ResponseMock::new();
    let mock = Mock::given(method("POST"))
        .and(path_regex(".*/responses/compact$"))
        .and(response_mock.clone());
    (mock, response_mock)
}

fn models_mock() -> (MockBuilder, ModelsMock) {
    let models_mock = ModelsMock::new();
    let mock = Mock::given(method("GET"))
        .and(path_regex(".*/models$"))
        .and(models_mock.clone());
    (mock, models_mock)
}

pub async fn mount_sse_once_match<M>(server: &MockServer, matcher: M, body: String) -> ResponseMock
where
    M: wiremock::Match + Send + Sync + 'static,
{
    let (mock, response_mock) = base_mock();
    mock.and(matcher)
        .respond_with(sse_response(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
    response_mock
}

pub async fn mount_sse_once(server: &MockServer, body: String) -> ResponseMock {
    let (mock, response_mock) = base_mock();
    mock.respond_with(sse_response(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
    response_mock
}

pub async fn mount_compact_json_once_match<M>(
    server: &MockServer,
    matcher: M,
    body: serde_json::Value,
) -> ResponseMock
where
    M: wiremock::Match + Send + Sync + 'static,
{
    let (mock, response_mock) = compact_mock();
    mock.and(matcher)
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(body.clone()),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
    response_mock
}

pub async fn mount_compact_json_once(server: &MockServer, body: serde_json::Value) -> ResponseMock {
    mount_compact_response_once(
        server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_json(body),
    )
    .await
}

/// Mount a `/responses/compact` mock that mirrors the default remote compaction shape:
/// keep user+developer messages from the request, drop assistant/tool artifacts, and append one
/// compaction item carrying the provided summary text.
pub async fn mount_compact_user_history_with_summary_once(
    server: &MockServer,
    summary_text: &str,
) -> ResponseMock {
    mount_compact_user_history_with_summary_sequence(server, vec![summary_text.to_string()]).await
}

/// Same as [`mount_compact_user_history_with_summary_once`], but for multiple compact calls.
/// Each incoming compact request receives the next summary text in order.
pub async fn mount_compact_user_history_with_summary_sequence(
    server: &MockServer,
    summary_texts: Vec<String>,
) -> ResponseMock {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    #[derive(Debug)]
    struct UserHistorySummaryResponder {
        num_calls: AtomicUsize,
        summary_texts: Vec<String>,
    }

    impl Respond for UserHistorySummaryResponder {
        fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
            let call_num = self.num_calls.fetch_add(1, Ordering::SeqCst);
            let Some(summary_text) = self.summary_texts.get(call_num) else {
                panic!("no summary text for compact request {call_num}");
            };
            let body_bytes = decode_body_bytes(
                &request.body,
                request
                    .headers
                    .get("content-encoding")
                    .and_then(|value| value.to_str().ok()),
            );
            let body_json: Value = serde_json::from_slice(&body_bytes)
                .unwrap_or_else(|err| panic!("failed to parse compact request body: {err}"));
            let mut output = body_json
                .get("input")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                // TODO(ccunningham): Update this mock to match future compaction model behavior:
                // return user/developer/assistant messages since the last compaction item, then
                // append a single newest compaction item.
                // Match current remote compaction behavior: keep user/developer messages and
                // omit assistant/tool history entries.
                .filter(|item| {
                    item.get("type").and_then(Value::as_str) == Some("message")
                        && matches!(
                            item.get("role").and_then(Value::as_str),
                            Some("user") | Some("developer")
                        )
                })
                .collect::<Vec<Value>>();
            // Append a synthetic compaction item as the newest item.
            output.push(serde_json::json!({
                "type": "compaction",
                "encrypted_content": summary_text,
            }));
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(serde_json::json!({ "output": output }))
        }
    }

    let num_calls = summary_texts.len();
    let responder = UserHistorySummaryResponder {
        num_calls: AtomicUsize::new(0),
        summary_texts,
    };
    let (mock, response_mock) = compact_mock();
    mock.respond_with(responder)
        .up_to_n_times(num_calls as u64)
        .expect(num_calls as u64)
        .mount(server)
        .await;
    response_mock
}

pub async fn mount_compact_response_once(
    server: &MockServer,
    response: ResponseTemplate,
) -> ResponseMock {
    let (mock, response_mock) = compact_mock();
    mock.respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
    response_mock
}

pub async fn mount_models_once(server: &MockServer, body: ModelsResponse) -> ModelsMock {
    let (mock, models_mock) = models_mock();
    mock.respond_with(
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_json(body.clone()),
    )
    .up_to_n_times(1)
    .mount(server)
    .await;
    models_mock
}

pub async fn mount_models_once_with_delay(
    server: &MockServer,
    body: ModelsResponse,
    delay: Duration,
) -> ModelsMock {
    let (mock, models_mock) = models_mock();
    mock.respond_with(
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_json(body.clone())
            .set_delay(delay),
    )
    .up_to_n_times(1)
    .mount(server)
    .await;
    models_mock
}

pub async fn mount_models_once_with_etag(
    server: &MockServer,
    body: ModelsResponse,
    etag: &str,
) -> ModelsMock {
    let (mock, models_mock) = models_mock();
    mock.respond_with(
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            // ModelsClient reads the ETag header, not a JSON field.
            .insert_header("ETag", etag)
            .set_body_json(body.clone()),
    )
    .up_to_n_times(1)
    .mount(server)
    .await;
    models_mock
}

pub async fn start_mock_server() -> MockServer {
    let server = MockServer::builder()
        .body_print_limit(BodyPrintLimit::Limited(80_000))
        .start()
        .await;

    // Provide a default `/models` response so tests remain hermetic when the client queries it.
    let _ = mount_models_once(&server, ModelsResponse { models: Vec::new() }).await;

    server
}


#[derive(Clone)]
pub struct FunctionCallResponseMocks {
    pub function_call: ResponseMock,
    pub completion: ResponseMock,
}

pub async fn mount_function_call_agent_response(
    server: &MockServer,
    call_id: &str,
    arguments: &str,
    tool_name: &str,
) -> FunctionCallResponseMocks {
    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, tool_name, arguments),
        ev_completed("resp-1"),
    ]);
    let function_call = mount_sse_once(server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    let completion = mount_sse_once(server, second_response).await;

    FunctionCallResponseMocks {
        function_call,
        completion,
    }
}

/// Mounts a sequence of SSE response bodies and serves them in order for each
/// POST to `/v1/responses`. Panics if more requests are received than bodies
/// provided. Also asserts the exact number of expected calls.
pub async fn mount_sse_sequence(server: &MockServer, bodies: Vec<String>) -> ResponseMock {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    struct SeqResponder {
        num_calls: AtomicUsize,
        responses: Vec<String>,
    }

    impl Respond for SeqResponder {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let call_num = self.num_calls.fetch_add(1, Ordering::SeqCst);
            match self.responses.get(call_num) {
                Some(body) => ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body.clone()),
                None => panic!("no response for {call_num}"),
            }
        }
    }

    let num_calls = bodies.len();
    let responder = SeqResponder {
        num_calls: AtomicUsize::new(0),
        responses: bodies,
    };

    let (mock, response_mock) = base_mock();
    mock.respond_with(responder)
        .up_to_n_times(num_calls as u64)
        .expect(num_calls as u64)
        .mount(server)
        .await;

    response_mock
}

/// Mounts a sequence of responses for each POST to `/v1/responses`.
/// Panics if more requests are received than responses provided.
pub async fn mount_response_sequence(
    server: &MockServer,
    responses: Vec<ResponseTemplate>,
) -> ResponseMock {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    struct SeqResponder {
        num_calls: AtomicUsize,
        responses: Vec<ResponseTemplate>,
    }

    impl Respond for SeqResponder {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let call_num = self.num_calls.fetch_add(1, Ordering::SeqCst);
            self.responses
                .get(call_num)
                .unwrap_or_else(|| panic!("no response for {call_num}"))
                .clone()
        }
    }

    let num_calls = responses.len();
    let responder = SeqResponder {
        num_calls: AtomicUsize::new(0),
        responses,
    };

    let (mock, response_mock) = base_mock();
    mock.respond_with(responder)
        .up_to_n_times(num_calls as u64)
        .expect(num_calls as u64)
        .mount(server)
        .await;
    response_mock
}

/// Validate invariants on the request body sent to `/v1/responses`.
///
/// - No `function_call_output`/`custom_tool_call_output` with missing/empty `call_id`.
/// - `tool_search_output` must have a `call_id` unless it is a server-executed legacy item.
/// - Every `function_call_output` must match a prior `function_call` or
///   `local_shell_call` with the same `call_id` in the same `input`.
/// - Every `custom_tool_call_output` must match a prior `custom_tool_call`.
/// - Every `tool_search_output` must match a prior `tool_search_call`.
/// - Additionally, enforce symmetry: every `function_call`/`custom_tool_call`/
///   `tool_search_call` in the `input` must have a matching output entry.
fn validate_request_body_invariants(request: &wiremock::Request) {
    // Skip GET requests (e.g., /models)
    if request.method != "POST" || !request.url.path().ends_with("/responses") {
        return;
    }
    let body_bytes = decode_body_bytes(
        &request.body,
        request
            .headers
            .get("content-encoding")
            .and_then(|value| value.to_str().ok()),
    );
    let Ok(body): Result<Value, _> = serde_json::from_slice(&body_bytes) else {
        return;
    };
    let Some(items) = body.get("input").and_then(Value::as_array) else {
        panic!("input array not found in request");
    };

    use std::collections::HashSet;

    fn get_call_id(item: &Value) -> Option<&str> {
        item.get("call_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    }

    fn gather_ids(items: &[Value], kind: &str) -> HashSet<String> {
        items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some(kind))
            .filter_map(get_call_id)
            .map(str::to_string)
            .collect()
    }

    fn gather_output_ids(items: &[Value], kind: &str, missing_msg: &str) -> HashSet<String> {
        items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some(kind))
            .map(|item| {
                let Some(id) = get_call_id(item) else {
                    panic!("{missing_msg}");
                };
                id.to_string()
            })
            .collect()
    }

    fn gather_tool_search_output_ids(items: &[Value]) -> HashSet<String> {
        items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_search_output"))
            .filter_map(|item| {
                if let Some(id) = get_call_id(item) {
                    return Some(id.to_string());
                }
                if item.get("execution").and_then(Value::as_str) == Some("server") {
                    return None;
                }
                panic!("orphan tool_search_output with empty call_id should be dropped");
            })
            .collect()
    }

    let function_calls = gather_ids(items, "function_call");
    let tool_search_calls = gather_ids(items, "tool_search_call");
    let custom_tool_calls = gather_ids(items, "custom_tool_call");
    let local_shell_calls = gather_ids(items, "local_shell_call");
    let function_call_outputs = gather_output_ids(
        items,
        "function_call_output",
        "orphan function_call_output with empty call_id should be dropped",
    );
    let tool_search_outputs = gather_tool_search_output_ids(items);
    let custom_tool_call_outputs = gather_output_ids(
        items,
        "custom_tool_call_output",
        "orphan custom_tool_call_output with empty call_id should be dropped",
    );

    for cid in &function_call_outputs {
        assert!(
            function_calls.contains(cid) || local_shell_calls.contains(cid),
            "function_call_output without matching call in input: {cid}",
        );
    }
    for cid in &custom_tool_call_outputs {
        assert!(
            custom_tool_calls.contains(cid),
            "custom_tool_call_output without matching call in input: {cid}",
        );
    }
    for cid in &tool_search_outputs {
        assert!(
            tool_search_calls.contains(cid),
            "tool_search_output without matching call in input: {cid}",
        );
    }

    for cid in &function_calls {
        assert!(
            function_call_outputs.contains(cid),
            "Function call output is missing for call id: {cid}",
        );
    }
    for cid in &custom_tool_calls {
        assert!(
            custom_tool_call_outputs.contains(cid),
            "Custom tool call output is missing for call id: {cid}",
        );
    }
    for cid in &tool_search_calls {
        assert!(
            tool_search_outputs.contains(cid),
            "Tool search output is missing for call id: {cid}",
        );
    }
}
