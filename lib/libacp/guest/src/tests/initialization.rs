use super::*;

#[tokio::test]
async fn initialization_timeout_still_closes_the_transport() {
    let (stream, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(stream);
    let mut peer = Peer::new(server);
    let (result, ()) = tokio::join!(
        AcpConnection::connect(
            reader,
            writer,
            ConnectionOptions {
                request_timeout: Duration::from_millis(100),
                ..Default::default()
            },
        ),
        async {
            assert_eq!(
                peer.receive().await.expect("initialize")["method"],
                "initialize"
            );
            while peer.receive().await.is_some() {}
        }
    );
    std::assert_matches!(result.map(|_| ()), Err(GuestError::Timeout));
}

#[tokio::test]
async fn zero_limits_are_rejected_without_starting_a_driver() {
    let (stream, _peer) = tokio::io::duplex(1024);
    let (reader, writer) = tokio::io::split(stream);
    let result = AcpConnection::connect(
        reader,
        writer,
        ConnectionOptions {
            notification_capacity: 0,
            ..Default::default()
        },
    )
    .await;
    std::assert_matches!(result.err(), Some(GuestError::InvalidOptions));
}

#[tokio::test]
async fn dropping_connect_during_initialization_closes_the_transport() {
    let (stream, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(stream);
    let mut peer = Peer::new(server);
    {
        let connecting = AcpConnection::connect(reader, writer, ConnectionOptions::default());
        tokio::pin!(connecting);
        tokio::select! {
            _ = &mut connecting => panic!("peer did not initialize"),
            submitted = peer.receive() => {
                assert_eq!(submitted.expect("initialize request")["method"], "initialize");
            }
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(2), peer.receive())
            .await
            .expect("cancelled constructor closes pipes")
            .is_none()
    );
}

#[tokio::test]
async fn incompatible_version_fails_and_closes_the_transport() {
    let (stream, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(stream);
    let mut peer = Peer::new(server);
    let (result, ()) = tokio::join!(
        AcpConnection::connect(reader, writer, ConnectionOptions::default()),
        async {
            let initialize = peer.receive().await.expect("initialize");
            peer.send(json!({
                "jsonrpc": "2.0", "id": initialize["id"],
                "result": {
                    "protocolVersion": 2,
                    "agentCapabilities": {},
                    "authMethods": []
                }
            }))
            .await;
        }
    );
    // The SDK now rejects incompatible initialize responses before decoding them.
    std::assert_matches!(
        result.err(),
        Some(GuestError::Protocol(protocol::Error {
            code: protocol::ErrorCode::InvalidRequest,
            ..
        }))
    );
    assert!(peer.receive().await.is_none());
}
