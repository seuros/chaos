use super::*;
use crate::error::ConnectionFailedError;
use tokio_stream::iter;

fn completed() -> ResponseEvent {
    ResponseEvent::Completed {
        response_id: "resp_1".to_string(),
        token_usage: None,
    }
}

#[tokio::test]
async fn compaction_stream_requires_exactly_one_item_and_completion() {
    let events = vec![
        Ok(ResponseEvent::OutputItemDone(ResponseItem::Compaction {
            encrypted_content: "encrypted".to_string(),
        })),
        Ok(completed()),
    ];

    let output = collect_compaction_output(iter(events))
        .await
        .expect("valid compaction stream");

    assert_eq!(
        output.item,
        ResponseItem::Compaction {
            encrypted_content: "encrypted".to_string()
        }
    );
}

#[tokio::test]
async fn compaction_stream_rejects_missing_output() {
    let error = collect_compaction_output(iter(vec![Ok(completed())]))
        .await
        .expect_err("missing compaction output must fail");

    assert!(error.to_string().contains("got 0"));
}

#[tokio::test]
async fn compaction_stream_rejects_multiple_outputs() {
    let item = ResponseItem::Compaction {
        encrypted_content: "encrypted".to_string(),
    };
    let error = collect_compaction_output(iter(vec![
        Ok(ResponseEvent::OutputItemDone(item.clone())),
        Ok(ResponseEvent::OutputItemDone(item)),
        Ok(completed()),
    ]))
    .await
    .expect_err("multiple compaction outputs must fail");

    assert!(error.to_string().contains("got 2"));
}

#[tokio::test]
async fn compaction_stream_requires_response_completed() {
    let error = collect_compaction_output(iter(vec![Ok(ResponseEvent::OutputItemDone(
        ResponseItem::Compaction {
            encrypted_content: "encrypted".to_string(),
        },
    ))]))
    .await
    .expect_err("incomplete compaction stream must fail");

    assert!(error.to_string().contains("before response.completed"));
}

#[test]
fn remote_compaction_retries_transient_connection_failures_within_budget() {
    let error = ChaosErr::ConnectionFailed(ConnectionFailedError {
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "operation timed out",
        )),
    });

    assert!(remote_compaction_retry_delay(&error, 1, 2).is_some());
    assert_eq!(remote_compaction_retry_delay(&error, 3, 2), None);
}

#[test]
fn v2_history_retains_messages_but_not_tool_transcript_or_trigger() {
    let user = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "keep me".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    let compaction = ResponseItem::Compaction {
        encrypted_content: "encrypted".to_string(),
    };
    let history = build_v2_compacted_history(
        vec![
            user.clone(),
            ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                namespace: None,
                arguments: "{}".to_string(),
                call_id: "call_1".to_string(),
                provider_metadata: None,
            },
            ResponseItem::CompactionTrigger {},
        ],
        compaction.clone(),
    );

    assert_eq!(history, vec![user, compaction]);
}

#[test]
fn v2_history_truncates_oversized_newest_message_instead_of_skipping_it() {
    let older = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "older".to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    let newest = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "newest ".repeat(100),
        }],
        end_turn: None,
        phase: None,
    };
    let compaction = ResponseItem::Compaction {
        encrypted_content: "encrypted".to_string(),
    };

    let history =
        build_v2_compacted_history_with_budget(vec![older, newest], compaction.clone(), 8);

    assert_eq!(history.len(), 2);
    let ResponseItem::Message { content, .. } = &history[0] else {
        panic!("expected retained newest user message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected retained user text");
    };
    assert!(text.contains("tokens truncated"));
    assert!(text.starts_with("newest"));
    assert_eq!(history[1], compaction);
}
