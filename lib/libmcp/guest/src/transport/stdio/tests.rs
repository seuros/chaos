use super::*;

#[tokio::test]
async fn shutdown_force_kills_and_reaps_a_child_that_ignores_stdin() {
    let args = vec!["-c".to_string(), "trap '' TERM; exec sleep 60".to_string()];
    let child =
        StdioChild::spawn("/bin/sh", &args, &HashMap::new(), None).expect("spawn stubborn child");
    let transport = StdioTransport::new(
        child,
        Duration::from_secs(1),
        Duration::from_millis(20),
        Duration::from_secs(1),
    );

    transport.shutdown().await.expect("shutdown transport");

    assert!(transport.reaped.load(Ordering::Acquire));
    assert!(
        transport
            .child
            .lock()
            .await
            .try_wait()
            .expect("query child status")
            .is_some(),
        "shutdown must not return before the child has been reaped"
    );
}

#[tokio::test]
async fn read_line_bounded_splits_lines_and_signals_eof() {
    let mut reader: &[u8] = b"one\ntwo\nlast";
    assert_eq!(
        read_line_bounded(&mut reader, 100)
            .await
            .unwrap()
            .as_deref(),
        Some("one")
    );
    assert_eq!(
        read_line_bounded(&mut reader, 100)
            .await
            .unwrap()
            .as_deref(),
        Some("two")
    );
    assert_eq!(
        read_line_bounded(&mut reader, 100)
            .await
            .unwrap()
            .as_deref(),
        Some("last")
    );
    assert!(read_line_bounded(&mut reader, 100).await.unwrap().is_none());
}

#[tokio::test]
async fn read_line_bounded_rejects_oversized_lines() {
    let data = vec![b'a'; 4096];
    let mut reader: &[u8] = &data;
    let error = read_line_bounded(&mut reader, 100).await.unwrap_err();
    assert!(matches!(error, GuestError::Protocol(_)), "got {error:?}");
}

#[tokio::test]
async fn recv_skips_non_jsonrpc_output_on_stdout() {
    let args = vec![
        "-c".to_string(),
        r#"printf 'accidental debug output\n{"jsonrpc":"2.0","method":"notifications/noise"}\n'"#
            .to_string(),
    ];
    let child = StdioChild::spawn("/bin/sh", &args, &HashMap::new(), None).expect("spawn child");
    let transport = StdioTransport::new(
        child,
        Duration::from_secs(1),
        Duration::from_millis(20),
        Duration::from_secs(1),
    );

    let message = transport.recv().await.expect("valid message after junk");
    let JsonRpcMessage::Notification(notification) = message else {
        panic!("expected notification, got {message:?}");
    };
    assert_eq!(notification.method, "notifications/noise");

    transport.force_shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn write_failure_closes_the_transport() {
    let args = vec!["-c".to_string(), "exit 0".to_string()];
    let child = StdioChild::spawn("/bin/sh", &args, &HashMap::new(), None).expect("spawn child");
    let transport = StdioTransport::new(
        child,
        Duration::from_secs(1),
        Duration::from_millis(20),
        Duration::from_secs(1),
    );

    transport
        .child
        .lock()
        .await
        .wait()
        .await
        .expect("child exits");

    let message =
        JsonRpcMessage::Notification(crate::protocol::JsonRpcRequest::notification("ping", None));
    let mut failed = false;
    for _ in 0..64 {
        if transport.send(message.clone()).await.is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed, "writes to a dead child must eventually fail");
    assert!(transport.closed.load(Ordering::Acquire));
    assert!(matches!(
        transport.send(message).await,
        Err(GuestError::Disconnected)
    ));

    transport.force_shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn force_shutdown_is_idempotent() {
    let args = vec!["-c".to_string(), "trap '' TERM; exec sleep 60".to_string()];
    let child =
        StdioChild::spawn("/bin/sh", &args, &HashMap::new(), None).expect("spawn stubborn child");
    let transport = StdioTransport::new(
        child,
        Duration::from_secs(1),
        Duration::from_millis(20),
        Duration::from_secs(1),
    );

    transport.force_shutdown().await.expect("first shutdown");
    transport.force_shutdown().await.expect("second shutdown");

    assert!(transport.reaped.load(Ordering::Acquire));
}
