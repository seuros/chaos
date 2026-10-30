use super::*;
use rama::futures;
use std::assert_matches;
use std::io;

#[tokio::test]
async fn done_and_eof_complete_once_after_fragmented_json_and_ignored_frames() {
    for terminal in ["", "data: [DONE]\n\ndata: {\"id\":\"after-done\"}\n\n"] {
        let stream = futures::stream::iter([
            Ok::<_, io::Error>(Bytes::from_static(
                b": keepalive\n\nevent: ping\n\ndata: not json\n\ndata: {\"id\":",
            )),
            Ok(Bytes::from_static(b"\"response\"}\n\n")),
            Ok(Bytes::from(terminal)),
        ]);
        let (tx, mut rx) = mpsc::channel(8);
        let mut parsed = 0;
        process_json_sse(stream, Duration::from_secs(1), tx, |json, response_id| {
            parsed += 1;
            *response_id = json["id"].as_str().expect("response id").to_string();
            Ok(vec![TurnEvent::OutputTextDelta("text".into())])
        })
        .await
        .expect("stream succeeds");

        assert_eq!(parsed, 1);
        assert_matches!(rx.try_recv(), Ok(Ok(TurnEvent::OutputTextDelta(text))) if text == "text");
        assert_matches!(
            rx.try_recv(),
            Ok(Ok(TurnEvent::Completed { response_id, token_usage: None }))
                if response_id == "response"
        );
        assert!(rx.try_recv().is_err(), "completion must not be duplicated");
    }
}

#[tokio::test]
async fn explicit_completion_stops_before_later_events_and_frames() {
    let stream = futures::stream::iter([Ok::<_, io::Error>(Bytes::from_static(
        b"data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
    ))]);
    let (tx, mut rx) = mpsc::channel(8);
    let mut parsed = 0;
    process_json_sse(stream, Duration::from_secs(1), tx, |_, _| {
        parsed += 1;
        Ok(vec![
            TurnEvent::Completed {
                response_id: "explicit".into(),
                token_usage: None,
            },
            TurnEvent::OutputTextDelta("after-completion".into()),
        ])
    })
    .await
    .expect("stream succeeds");
    assert_eq!(parsed, 1);
    assert_matches!(
        rx.try_recv(),
        Ok(Ok(TurnEvent::Completed { response_id, .. })) if response_id == "explicit"
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn parser_errors_propagate_without_fallback_completion() {
    let stream = futures::stream::iter([Ok::<_, io::Error>(Bytes::from_static(b"data: {}\n\n"))]);
    let (tx, mut rx) = mpsc::channel(8);
    let result = process_json_sse(stream, Duration::from_secs(1), tx, |_, _| {
        Err(AbiError::Transport {
            status: 429,
            message: "provider error".into(),
        })
    })
    .await;
    assert_matches!(result, Err(AbiError::Transport { status: 429, .. }));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn transport_errors_and_idle_timeouts_do_not_complete() {
    let stream = futures::stream::iter([Err::<Bytes, _>(io::Error::other("broken stream"))]);
    let (tx, mut rx) = mpsc::channel(8);
    let result = process_json_sse(stream, Duration::from_secs(1), tx, |_, _| Ok(Vec::new())).await;
    assert_matches!(result, Err(AbiError::Stream(message)) if message.contains("broken stream"));
    assert!(rx.try_recv().is_err());

    let stream = futures::stream::pending::<Result<Bytes, io::Error>>();
    let (tx, mut rx) = mpsc::channel(8);
    let result =
        process_json_sse(stream, Duration::from_millis(1), tx, |_, _| Ok(Vec::new())).await;
    assert_matches!(
        result,
        Err(AbiError::Stream(message)) if message == "idle timeout waiting for SSE"
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn disconnected_receiver_stops_parsing_after_first_send() {
    let stream = futures::stream::iter([Ok::<_, io::Error>(Bytes::from_static(
        b"data: {}\n\ndata: {}\n\n",
    ))]);
    let (tx, rx) = mpsc::channel(8);
    drop(rx);
    let mut parsed = 0;
    process_json_sse(stream, Duration::from_secs(1), tx, |_, _| {
        parsed += 1;
        Ok(vec![TurnEvent::Created])
    })
    .await
    .expect("disconnection is not a provider failure");
    assert_eq!(parsed, 1);
}
