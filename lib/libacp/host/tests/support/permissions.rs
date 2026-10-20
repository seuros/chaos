use super::*;
use serde_json::json;

use super::faults::open;

#[tokio::test]
async fn default_guest_permission_denial_never_authorizes_tool_execution() {
    let (connection, mut updates, task, _) = start(ConnectionLimits::default()).await;
    let id = session(&connection).await;
    connection
        .client()
        .request(prompt(&id, "permission"))
        .await
        .unwrap();
    let first = serde_json::to_value(updates.recv().await.unwrap()).unwrap();
    assert_eq!(first["update"]["content"]["text"], "denied");
    updates.recv().await.unwrap();
    connection.disconnect().await.unwrap();
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn only_an_offered_allow_once_selection_can_authorize() {
    for (selection, expected) in [
        ("allow", "allowed"),
        ("deny", "denied"),
        ("injected", "denied"),
    ] {
        let (mut peer, task, _, id) = open(ConnectionLimits::default()).await;
        peer.prompt(3, &id, "permission").await;
        let permission = peer.receive().await;
        assert_eq!(permission["method"], "session/request_permission");
        assert_eq!(permission["params"]["sessionId"], id);
        peer.send(json!({
            "jsonrpc":"2.0","id":permission["id"],
            "result":{"outcome":{"outcome":"selected","optionId":selection}}
        }))
        .await;
        assert_eq!(
            peer.receive().await["params"]["update"]["content"]["text"],
            expected
        );
        assert_eq!(
            peer.receive().await["params"]["update"]["content"]["text"],
            "1:permission"
        );
        assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
        drop(peer);
        let _ = task.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_during_permission_and_stale_allow_cannot_leak_into_next_turn() {
    let (mut peer, task, _, id) = open(ConnectionLimits::default()).await;
    peer.prompt(3, &id, "permission").await;
    let old = peer.receive().await;
    peer.send(json!({
        "jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}
    }))
    .await;
    assert_eq!(peer.receive().await["result"]["stopReason"], "cancelled");
    peer.send(json!({
        "jsonrpc":"2.0","id":old["id"],
        "result":{"outcome":{"outcome":"selected","optionId":"allow"}}
    }))
    .await;
    peer.prompt(4, &id, "after").await;
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "2:after"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    drop(peer);
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn permission_deadline_denies_only_that_request_and_retains_its_credit() {
    let (mut peer, task, observations, id) = open(ConnectionLimits {
        approval_timeout: Duration::from_millis(20),
        pending_requests: 2,
        ..Default::default()
    })
    .await;
    peer.prompt(3, &id, "permission").await;
    let old = peer.receive().await;
    assert_eq!(old["method"], "session/request_permission");
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "denied"
    );
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "1:permission"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 0);

    peer.prompt(4, &id, "permission").await;
    assert_eq!(peer.receive().await["method"], "session/request_permission");
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "denied"
    );
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "2:permission"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    peer.prompt(5, &id, "permission").await;
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "denied"
    );
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "3:permission"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    peer.prompt(6, &id, "after").await;
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "4:after"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    peer.send(json!({
        "jsonrpc":"2.0","id":old["id"],
        "result":{"outcome":{"outcome":"selected","optionId":"allow"}}
    }))
    .await;
    peer.prompt(7, &id, "permission").await;
    let fresh = peer.receive().await;
    assert_eq!(fresh["method"], "session/request_permission");
    assert_ne!(fresh["id"], old["id"]);
    peer.send(json!({
        "jsonrpc":"2.0","id":fresh["id"],
        "result":{"outcome":{"outcome":"cancelled"}}
    }))
    .await;
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "denied"
    );
    assert_eq!(
        peer.receive().await["params"]["update"]["content"]["text"],
        "5:permission"
    );
    assert_eq!(peer.receive().await["result"]["stopReason"], "end_turn");
    drop(peer);
    assert!(task.await.unwrap().is_ok());
    assert_eq!(observations.shutdowns.load(Ordering::SeqCst), 1);
}
