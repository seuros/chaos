use super::ResponseItem;

#[test]
fn compaction_trigger_serializes_as_request_control() {
    let value = serde_json::to_value(ResponseItem::CompactionTrigger {})
        .expect("compaction trigger should serialize");

    assert_eq!(value, serde_json::json!({"type": "compaction_trigger"}));
}

#[test]
fn compaction_trigger_is_not_accepted_as_response_data() {
    let item: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "compaction_trigger"
    }))
    .expect("unknown response items should deserialize safely");

    assert_eq!(item, ResponseItem::Other);
}
