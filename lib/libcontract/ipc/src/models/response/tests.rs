use super::ResponseItem;

#[test]
fn compaction_trigger_is_outbound_request_control_only() {
    let wire = serde_json::json!({"type": "compaction_trigger"});
    let value = serde_json::to_value(ResponseItem::CompactionTrigger {})
        .expect("compaction trigger should serialize");

    assert_eq!(value, wire, "outbound request control");
    let item: ResponseItem =
        serde_json::from_value(wire).expect("unknown response items should deserialize safely");

    assert_eq!(item, ResponseItem::Other, "inbound response data");
}
