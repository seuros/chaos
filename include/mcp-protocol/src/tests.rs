use super::*;
use serde_json::json;

fn hint(uri: &str, message_ids: Vec<&str>) -> Value {
    json!({ "uri": uri, "message_ids": message_ids })
}

#[test]
fn parse_canonicalizes_valid_hints_and_rejects_the_rest() {
    let parsed =
        FleetInboxHint::parse(hint(FLEET_INBOX_URI, vec!["3", "1", "2", "1"])).expect("valid hint");
    assert_eq!(parsed.message_ids, vec!["1", "2", "3"]);

    assert!(FleetInboxHint::parse(hint("skynet://fleet/inbox", vec!["1"])).is_none());
    assert!(FleetInboxHint::parse(hint(FLEET_INBOX_URI, vec![])).is_none());
    assert!(FleetInboxHint::parse(hint(FLEET_INBOX_URI, vec!["01"])).is_none());
    assert!(
        FleetInboxHint::parse(json!({
            "uri": FLEET_INBOX_URI,
            "message_ids": ["1"],
            "extra": true
        }))
        .is_none()
    );

    let mut ids: Vec<String> = (1..=50).map(|n| n.to_string()).collect();
    ids.push("1".into());
    assert_eq!(
        FleetInboxHint::parse(json!({
            "uri": FLEET_INBOX_URI,
            "message_ids": ids
        }))
        .expect("duplicates collapse before the unique cap")
        .message_ids
        .len(),
        50
    );
    ids.push("51".into());
    assert!(
        FleetInboxHint::parse(json!({
            "uri": FLEET_INBOX_URI,
            "message_ids": ids
        }))
        .is_none()
    );
}
