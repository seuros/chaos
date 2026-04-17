//! Live xAI Batch API round-trip.
//!
//! `#[ignore]` by default; runs with
//! `cargo test -p chaos-parrot --test xai_spool_live -- --ignored`
//! when `XAI_API_KEY` is set.

use std::time::Duration;

use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::SpoolBackend;
use chaos_abi::SpoolPhase;
use chaos_abi::TurnRequest;
use chaos_abi::TurnResult;
use chaos_parrot::endpoint::batches::XaiSpoolBackend;
use serde_json::Map;
use tokio::time::sleep;

const POLL_INTERVAL: Duration = Duration::from_secs(15);
const OVERALL_BUDGET: Duration = Duration::from_secs(30 * 60);

fn build_request(prompt: &str) -> TurnRequest {
    TurnRequest {
        model: String::new(),
        instructions: "Reply with a single short sentence.".into(),
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: prompt.into(),
            }],
            end_turn: None,
            phase: None,
        }],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: Map::new(),
    }
}

#[tokio::test]
#[ignore = "requires XAI_API_KEY and spends real credit"]
async fn xai_batch_roundtrip() {
    let api_key =
        std::env::var("XAI_API_KEY").expect("XAI_API_KEY unset — skip by omitting --ignored");
    let backend = XaiSpoolBackend::new(api_key, "grok-4-1-fast".into());

    let items = vec![
        ("ping-one".to_string(), build_request("Say 'one'.")),
        ("ping-two".to_string(), build_request("Say 'two'.")),
    ];
    let expected_ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();

    let batch_id = backend.submit(items).await.expect("submit failed");
    eprintln!("submitted batch {batch_id}");

    let deadline = std::time::Instant::now() + OVERALL_BUDGET;
    let phase = loop {
        let report = backend.poll(&batch_id).await.expect("poll failed");
        eprintln!("  {}: {:?}", report.raw_provider_status, report.phase);
        if report.phase != SpoolPhase::InProgress {
            break report.phase;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "batch {batch_id} still in progress after {OVERALL_BUDGET:?}"
        );
        sleep(POLL_INTERVAL).await;
    };
    assert_eq!(phase, SpoolPhase::Completed, "batch did not complete");

    let results = backend
        .fetch_results(&batch_id)
        .await
        .expect("fetch_results failed");
    assert_eq!(results.len(), expected_ids.len(), "result count mismatch");

    let mut got_ids: Vec<String> = results.iter().map(|(id, _)| id.clone()).collect();
    got_ids.sort();
    let mut want_ids = expected_ids.clone();
    want_ids.sort();
    assert_eq!(got_ids, want_ids, "batch_request_ids did not round-trip");

    for (id, result) in &results {
        match result {
            TurnResult::Success(out) => assert!(!out.content.is_empty(), "empty content for {id}"),
            TurnResult::Error(err) => panic!("{id} errored: {} {}", err.code, err.message),
        }
    }
}
