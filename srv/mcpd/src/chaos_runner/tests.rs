use super::*;
use pretty_assertions::assert_eq;

#[test]
fn session_outcome_captures_process_id() {
    let process_id = ProcessId::new();
    let outcome = SessionOutcome {
        process_id,
        text: "done".to_string(),
        is_error: false,
    };
    assert_eq!(outcome.process_id, process_id);
    assert_eq!(outcome.text, "done");
    assert!(!outcome.is_error);
}
