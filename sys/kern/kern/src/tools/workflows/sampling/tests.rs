use super::*;
use crate::client_common::{ResponseEvent, ResponseStream};
use rama::futures::FutureExt;
use serde_json::json;
use tokio::sync::mpsc;

mod provider;

fn params() -> CreateMessageParams {
    serde_json::from_value(json!({
        "messages":[{"role":"user","content":{"type":"text","text":"Say hello"}}],
        "maxTokens":32
    }))
    .unwrap()
}

fn stream(events: Vec<ResponseEvent>) -> ResponseStream {
    let (tx, rx_event) = mpsc::channel(events.len().max(1));
    for event in events {
        tx.try_send(Ok(event)).unwrap();
    }
    ResponseStream { rx_event }
}

fn message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some("message".into()),
        role: "assistant".into(),
        content: vec![ContentItem::OutputText { text: text.into() }],
        end_turn: None,
        phase: None,
    }
}

fn completed() -> ResponseEvent {
    ResponseEvent::Completed {
        response_id: "reply".into(),
        token_usage: None,
    }
}

#[test]
fn sampling_rejects_unbounded_or_extended_inputs_before_dispatch() {
    assert_eq!(validate(&params()).unwrap().get(), 32);
    for tokens in [0, MAX_SAMPLE_TOKENS + 1, u32::MAX] {
        let mut input = params();
        input.max_tokens = tokens;
        assert!(validate(&input).is_err());
    }
    for (field, value) in [
        ("tools", json!([])),
        ("modelPreferences", json!({})),
        ("includeContext", json!("allServers")),
        ("temperature", json!(0.1)),
        ("stopSequences", json!([])),
        ("_meta", json!({"source":"direct"})),
        ("metadata", json!({})),
    ] {
        let mut value_with_field = serde_json::to_value(params()).unwrap();
        value_with_field[field] = value;
        let input: CreateMessageParams = serde_json::from_value(value_with_field).unwrap();
        assert!(validate(&input).is_err(), "{field}");
    }
    let mut oversized = params();
    oversized.system_prompt = Some("x".repeat(MAX_SAMPLE_INPUT_BYTES));
    assert!(validate(&oversized).is_err());
}

#[tokio::test]
async fn collector_does_not_duplicate_deltas_and_final_message() {
    let output = output::collect(stream(vec![
        ResponseEvent::Created,
        ResponseEvent::OutputItemAdded(message("")),
        ResponseEvent::OutputTextDelta("hello".into()),
        ResponseEvent::OutputItemDone(message("hello")),
        completed(),
    ]))
    .await
    .unwrap();
    assert_eq!(output.text, "hello");
    let output = output::collect(stream(vec![
        ResponseEvent::OutputItemAdded(message("")),
        ResponseEvent::OutputTextDelta("deltas only".into()),
        completed(),
    ]))
    .await
    .unwrap();
    assert_eq!(output.text, "deltas only");
}

#[tokio::test]
async fn collector_requires_completion_and_rejects_actions_at_announcement() {
    assert!(
        output::collect(stream(vec![ResponseEvent::OutputTextDelta(
            "unfinished".into()
        ),]))
        .await
        .is_err()
    );
    let action = ResponseItem::FunctionCall {
        id: None,
        name: "exec_command".into(),
        namespace: None,
        arguments: "{}".into(),
        call_id: "forged".into(),
        provider_metadata: None,
    };
    let error = output::collect(stream(vec![
        ResponseEvent::OutputItemAdded(action),
        completed(),
    ]))
    .await
    .err()
    .unwrap();
    assert!(error.contains("refused a provider tool/action"));
}

#[tokio::test]
async fn collector_bounds_provider_bytes_and_event_count() {
    assert!(
        output::collect(stream(vec![
            ResponseEvent::ReasoningContentDelta {
                delta: "x".repeat(256 * 1024),
                content_index: 0
            },
            completed(),
        ]))
        .await
        .is_err()
    );
    assert!(
        output::collect(stream((0..4097).map(|_| ResponseEvent::Created).collect()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dropping_collector_releases_silent_provider_receiver() {
    let (tx, rx_event) = mpsc::channel(1);
    let mut collect = Box::pin(output::collect(ResponseStream { rx_event }));
    assert!(collect.as_mut().now_or_never().is_none());
    assert!(!tx.is_closed());
    drop(collect);
    assert!(tx.is_closed());
}

async fn caller() -> Caller {
    let (session, turn, tracker) = super::super::tests::fixture().await;
    session
        .services
        .tool_group_catalog
        .set_groups_enabled(
            &session.services.tool_group_state,
            [crate::tools::groups::WORKFLOWS],
            true,
        )
        .unwrap();
    Caller {
        invocation: std::sync::Arc::new(super::super::tests::invocation(
            session,
            turn,
            tracker,
            "run_workflow",
            json!({}),
        )),
        cancellation: tokio_util::sync::CancellationToken::new(),
        run_id: chaos_synopsis::runs::RunRegistry::default()
            .start(chaos_synopsis::runs::RunKind::Workflow)
            .unwrap()
            .id(),
        tasks: tokio_util::task::TaskTracker::new(),
    }
}

#[tokio::test]
async fn queued_sample_rechecks_group_and_cancel_before_provider_dispatch() {
    for cancel in [false, true] {
        let caller = caller().await;
        let session = &caller.invocation.session;
        let held = session
            .services
            .tool_scheduler
            .acquire(false)
            .await
            .unwrap();
        let mut sample = Box::pin(super::sample(&caller, params()));
        assert!(sample.as_mut().now_or_never().is_none());
        if cancel {
            caller.cancellation.cancel();
        } else {
            session
                .services
                .tool_group_catalog
                .set_groups_enabled(
                    &session.services.tool_group_state,
                    [crate::tools::groups::WORKFLOWS],
                    false,
                )
                .unwrap();
        }
        drop(held);
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), sample)
            .await
            .unwrap()
            .err()
            .unwrap();
        assert!(
            error.contains(if cancel {
                "cancelled"
            } else {
                "no longer enabled"
            }),
            "{error}"
        );
    }
}

#[tokio::test]
async fn auxiliary_usage_preserves_canonical_context_baseline() {
    let caller = caller().await;
    let session = &caller.invocation.session;
    let turn = &caller.invocation.turn;
    let baseline = chaos_ipc::protocol::TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        total_tokens: 120,
        ..Default::default()
    };
    session.update_token_usage_info(turn, Some(&baseline)).await;
    session
        .record_auxiliary_token_usage(
            turn,
            Some(&chaos_ipc::protocol::TokenUsage {
                input_tokens: 3,
                output_tokens: 2,
                total_tokens: 5,
                ..Default::default()
            }),
        )
        .await;
    let info = session.state.lock().await.token_info().unwrap();
    assert_eq!(info.last_token_usage, baseline);
    assert_eq!(info.total_token_usage.total_tokens, 125);
}
