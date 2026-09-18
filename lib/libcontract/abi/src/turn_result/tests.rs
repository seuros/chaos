use super::*;

#[test]
fn turn_result_success_round_trips_with_internal_tag() {
    let value = serde_json::to_value(TurnResult::Success(TurnOutput {
        content: vec![ContentItem::OutputText { text: "hi".into() }],
        finish_reason: Some("end_turn".into()),
        usage: None,
        server_model: Some("mock-model".into()),
    }))
    .expect("serialize success");

    assert_eq!(value["outcome"], "success");
    assert_eq!(value["finish_reason"], "end_turn");
    assert_eq!(value["server_model"], "mock-model");

    let parsed: TurnResult = serde_json::from_value(value).expect("deserialize success");
    let TurnResult::Success(output) = parsed else {
        panic!("expected success");
    };
    assert_eq!(
        output.content,
        vec![ContentItem::OutputText { text: "hi".into() }]
    );
    assert_eq!(output.finish_reason.as_deref(), Some("end_turn"));
    assert_eq!(output.server_model.as_deref(), Some("mock-model"));
}

#[test]
fn turn_result_error_round_trips_with_internal_tag() {
    let value = serde_json::to_value(TurnResult::Error(TurnError {
        code: "rate_limit".into(),
        message: "too many requests".into(),
        usage: None,
    }))
    .expect("serialize error");

    assert_eq!(value["outcome"], "error");
    assert_eq!(value["code"], "rate_limit");
    assert_eq!(value["message"], "too many requests");

    let parsed: TurnResult = serde_json::from_value(value).expect("deserialize error");
    let TurnResult::Error(err) = parsed else {
        panic!("expected error");
    };
    assert_eq!(err.code, "rate_limit");
    assert_eq!(err.message, "too many requests");
    assert!(err.usage.is_none());
}
