use super::*;
use serde_json::json;

#[test]
fn observed_context_window_records_round_trip_and_validate_windows() {
    let mut record: ObservedModelContextWindowItem = serde_json::from_value(json!({
        "provider": "account-a",
        "model": "unknown-model",
        "context_window": 200000,
        "scope": {
            "wire_api": "responses",
            "base_url": "https://example.com/v1"
        }
    }))
    .expect("scoped record");
    let observation = ContextWindowObservation::from_record(&record).expect("scoped record");
    assert_eq!(observation.to_record(), record);

    for window in [0, -1] {
        record.context_window = window;
        assert!(ContextWindowObservation::from_record(&record).is_none());
    }
}
