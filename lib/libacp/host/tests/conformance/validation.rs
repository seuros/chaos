use super::*;

#[tokio::test]
async fn negotiated_boolean_controls_accept_and_return_boolean_values() {
    let (mut peer, _) = Peer::new(json!({"session":{"configOptions":{"boolean":{}}}})).await;
    peer.request(
        2,
        "session/new",
        json!({"cwd":"/workspace","mcpServers":[]}),
    )
    .await;
    peer.receive().await;
    peer.request(
        3,
        "session/set_config_option",
        json!({"sessionId":"s","configId":"flag","type":"boolean","value":false}),
    )
    .await;
    let update = peer.receive().await;
    assert_eq!(
        update["params"]["update"]["configOptions"][0]["currentValue"],
        false
    );
    peer.receive().await; // current_mode_update
    assert_eq!(
        peer.receive().await["result"]["configOptions"][0]["currentValue"],
        false
    );
    peer.finish().await;
}

#[tokio::test]
async fn unknown_and_generic_cancellation_notifications_do_not_leak_credits() {
    let (mut peer, _) = Peer::new(json!({})).await;
    for id in 2..100 {
        peer.send(json!({"jsonrpc":"2.0","method":"_ignored","params":{}}))
            .await;
        peer.send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":"not-pending"}})).await;
        peer.send(
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"unknown"}}),
        )
        .await;
        peer.request(id, "_ping", json!({})).await;
        let response = peer.receive().await;
        assert_eq!(response["id"], id);
        assert_eq!(response["error"]["code"], -32601);
    }
    peer.finish().await;
}

#[tokio::test]
async fn relative_workspace_paths_are_rejected_before_backend_work_on_every_lifecycle() {
    for method in ["session/new", "session/load", "session/resume"] {
        let (mut peer, _) = Peer::new(json!({})).await;
        for (id, cwd, roots) in [
            (2, "relative", json!([])),
            (3, "/workspace", json!(["relative"])),
        ] {
            peer.request(
                id,
                method,
                json!({"sessionId":"s","cwd":cwd,"additionalDirectories":roots,"mcpServers":[]}),
            )
            .await;
            assert_eq!(peer.receive().await["error"]["code"], -32602);
        }
        assert_eq!(peer.backend.opens.load(Ordering::SeqCst), 0);
        peer.finish().await;
    }
}
