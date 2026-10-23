use super::*;
use chaos_ipc::protocol::TurnProgressEvent;

#[test]
fn transient_catalog_and_progress_events_are_not_persisted() {
    for event in [
        EventMsg::ListModelsResponse(chaos_ipc::protocol::ListModelsResponseEvent {
            models: Vec::new(),
        }),
        EventMsg::TurnProgress(TurnProgressEvent {
            turn_id: "turn-1".to_string(),
            approx_reasoning_tokens: 10,
            approx_output_tokens: 5,
            approx_total_tokens: 15,
        }),
    ] {
        for mode in [
            EventPersistenceMode::Limited,
            EventPersistenceMode::Extended,
        ] {
            assert!(!should_persist_event_msg(&event, mode), "{event:?}");
        }
    }
}
