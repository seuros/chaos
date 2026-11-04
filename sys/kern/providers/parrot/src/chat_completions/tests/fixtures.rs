use serde_json::Value;

pub(super) fn completion_fixture() -> Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "model": "server-model",
        "choices": [{
            "message": {"role": "assistant", "content": "{\"ok\":true}"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 17}
    })
}
