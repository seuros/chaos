use super::*;
use chaos_kern::activity::{Activity, ActivityEvent, CallOutcome, Station};
use rama::http::sse::EventDataWrite as _;

#[test]
fn activity_reaches_the_log_as_its_redacted_contract() {
    let event = ActivityEvent {
        process_id: chaos_ipc::ProcessId::new(),
        at_ms: 1,
        activity: Activity::CallEnded {
            call_id: "call-1".to_string(),
            station: Station::Network,
            label: "<curl>".to_string(),
            outcome: CallOutcome::Failed,
            duration_ms: Some(42),
            check: None,
        },
    };
    let mut frame = Vec::new();
    activity_line(&event)
        .unwrap()
        .into_data()
        .unwrap()
        .write_data(&mut frame)
        .unwrap();
    let frame = String::from_utf8(frame).unwrap();
    assert!(frame.contains("selector #monitor-log"), "{frame}");
    assert!(frame.contains("mode append"), "{frame}");
    assert!(
        frame.contains("&quot;station&quot;:&quot;network&quot;"),
        "{frame}"
    );
    assert!(frame.contains("&lt;curl&gt;"), "{frame}");
}
