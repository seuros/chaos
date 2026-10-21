use super::*;
use core_test_support::responses::ev_apply_patch_custom_tool_call;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_preserves_history_delete_archives_it_and_resume_cannot_resurrect_it() {
    let fixture = Fixture::new().await;
    let requests = mount_sse_sequence(&fixture.server, vec![response("r1", "saved")]).await;
    let (mut peer, id) = fixture.peer().await;
    let prompt = peer.prompt(&id, "remember").await;
    peer.finish(&id, prompt, "end_turn").await;
    let (active_delete, _) = peer
        .request("session/delete", json!({"sessionId":id}))
        .await;
    assert!(active_delete["error"].is_object());
    let (closed, _) = peer.request("session/close", json!({"sessionId":id})).await;
    assert!(closed["result"].is_object(), "{closed}");
    let (listed, _) = peer.request("session/list", json!({})).await;
    assert!(
        listed["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["sessionId"] == id)
    );
    let (resumed, _) = peer
        .request(
            "session/resume",
            json!({"sessionId":id, "cwd":fixture.cwd.path(),"mcpServers":[]}),
        )
        .await;
    assert!(resumed["result"].is_object(), "{resumed}");
    peer.request("session/close", json!({"sessionId":id})).await;
    for _ in 0..2 {
        let (deleted, _) = peer
            .request("session/delete", json!({"sessionId":id}))
            .await;
        assert!(deleted["result"].is_object(), "{deleted}");
    }
    peer.close().await;
    let (mut other, _) = fixture.peer().await;
    let (listed, _) = other.request("session/list", json!({})).await;
    assert!(
        !listed["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["sessionId"] == id)
    );
    for method in ["session/resume", "session/load"] {
        let (result, _) = other
            .request(
                method,
                json!({"sessionId":id,"cwd":fixture.cwd.path(),"mcpServers":[]}),
            )
            .await;
        assert!(result["error"].is_object(), "{result}");
    }
    assert_eq!(
        requests.requests().len(),
        1,
        "lifecycle calls never execute saved work"
    );
    other.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_connection_delete_cannot_seize_a_writer_and_close_joins_pending_approval() {
    let mut fixture = Fixture::new().await;
    fixture.supervised();
    let (path, patch) = fixture.patch("closed-prompt.txt", "forbidden");
    mount_sse_sequence(
        &fixture.server,
        vec![sse(vec![
            ev_response_created("close-r1"),
            ev_apply_patch_custom_tool_call("pending-patch", &patch),
            ev_completed("close-r1"),
        ])],
    )
    .await;
    let (mut peer, id) = fixture.peer().await;
    let mut contender = Peer::connect(fixture.config.clone()).await;
    let (denied, _) = contender
        .request("session/delete", json!({"sessionId":id}))
        .await;
    assert!(denied["error"].is_object(), "{denied}");
    contender.close().await;
    let prompt = peer.prompt(&id, "propose an edit").await;
    peer.permission(&id).await;
    let close = peer.next;
    peer.next += 1;
    peer.send(
        json!({"jsonrpc":"2.0","id":close,"method":"session/close","params":{"sessionId":id}}),
    )
    .await;
    let mut cancelled = false;
    let mut closed = false;
    while !cancelled || !closed {
        let frame = peer.receive().await;
        if frame["id"] == prompt {
            assert_eq!(frame["result"]["stopReason"], "cancelled", "{frame}");
            cancelled = true;
        } else if frame["id"] == close {
            assert!(frame["result"].is_object(), "{frame}");
            closed = true;
        } else {
            assert_eq!(frame["method"], "session/update", "{frame}");
        }
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), "before\n");
    let (deleted, _) = peer
        .request("session/delete", json!({"sessionId":id}))
        .await;
    assert!(deleted["result"].is_object(), "{deleted}");
    peer.close().await;
}
