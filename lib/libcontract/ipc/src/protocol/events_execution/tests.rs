use super::*;

#[test]
fn exec_sources_use_canonical_wire_names() {
    for (source, name) in [
        (ExecCommandSource::Agent, "agent"),
        (ExecCommandSource::UserShell, "user_shell"),
        (ExecCommandSource::ExecStartup, "exec_startup"),
        (ExecCommandSource::ExecInteraction, "exec_interaction"),
    ] {
        let value = serde_json::to_value(source).unwrap();
        assert_eq!(value, name);
        assert_eq!(
            serde_json::from_value::<ExecCommandSource>(value).unwrap(),
            source
        );
    }

    let schema = serde_json::to_value(schemars::schema_for!(ExecCommandSource)).unwrap();
    assert_eq!(
        schema["enum"],
        serde_json::json!(["agent", "user_shell", "exec_startup", "exec_interaction"])
    );
}

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
