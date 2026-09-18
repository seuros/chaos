use super::*;

#[test]
fn output_delta_uses_padded_base64_and_accepts_unpadded_input() {
    let event = ExecCommandOutputDeltaEvent {
        call_id: "call-1".to_owned(),
        stream: ExecOutputStream::Stdout,
        chunk: b"Hello World".to_vec(),
    };

    let mut value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["chunk"], "SGVsbG8gV29ybGQ=");

    value["chunk"] = "SGVsbG8gV29ybGQ".into();
    let decoded: ExecCommandOutputDeltaEvent = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, event);
}
