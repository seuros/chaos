use super::*;
use core_test_support::responses::ev_apply_patch_custom_tool_call;

fn patch_response(call: &str, patch: &str) -> String {
    sse(vec![
        ev_response_created("r1"),
        ev_apply_patch_custom_tool_call(call, patch),
        ev_completed("r1"),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_patch_allow_and_deny_once() {
    for allow in [true, false] {
        let mut fixture = Fixture::new().await;
        let (file, patch) = fixture.patch("private-fixture.txt", "after");
        mount_sse_sequence(
            &fixture.server,
            vec![
                patch_response("private-patch", &patch),
                response("r2", "done"),
            ],
        )
        .await;
        fixture.supervised();
        let (mut peer, session) = fixture.peer().await;
        let prompt = peer.prompt(&session, "edit the private fixture").await;
        let mut approved = 0;
        let mut saw_tool = false;
        loop {
            let frame = peer.receive().await;
            match frame["method"].as_str() {
                Some("session/request_permission") => {
                    approved += 1;
                    assert_eq!(frame["params"]["sessionId"], session);
                    for option in frame["params"]["options"].as_array().unwrap() {
                        assert!(
                            ["allow_once", "reject_once"]
                                .contains(&option["kind"].as_str().unwrap())
                        );
                    }
                    peer.reply_permission(&frame, if allow { "allow" } else { "deny" })
                        .await;
                }
                Some("session/update") => {
                    assert_eq!(frame["params"]["sessionId"], session);
                    saw_tool |= frame["params"]["update"]["sessionUpdate"] == "tool_call";
                }
                _ => {
                    assert_eq!(frame["id"], prompt);
                    assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                    break;
                }
            }
        }
        assert_eq!(approved, 1);
        assert!(saw_tool);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            if allow { "after\n" } else { "before\n" }
        );
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_at_native_approval_then_fresh_turn_rejects_stale_allow() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::{Mock, ResponseTemplate, matchers};

    let mut fixture = Fixture::new().await;
    let (file, patch) = fixture.patch("cancelled-private-fixture.txt", "forbidden");
    let first = patch_response("cancelled-patch", &patch);
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/v1/responses"))
        .respond_with(move |_: &wiremock::Request| {
            let body = if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                first.clone()
            } else {
                response("r-next", "fresh native response")
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
        })
        .mount(&fixture.server)
        .await;
    fixture.supervised();
    let (mut peer, session) = fixture.peer().await;
    let prompt = peer.prompt(&session, "edit then cancel").await;
    let permission = peer.permission(&session).await;
    peer.cancel(&session).await;
    peer.finish(&session, prompt, "cancelled").await;
    peer.reply_permission(&permission, "allow").await;
    let fresh = peer.prompt(&session, "fresh user turn").await;
    let updates = peer.finish(&session, fresh, "end_turn").await;
    let text: String = updates
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .map(|frame| {
            frame["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(text, "fresh native response");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
    assert!((2..=3).contains(&count.load(Ordering::SeqCst)));
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eof_denies_pending_native_patch_and_joins_process() {
    let mut fixture = Fixture::new().await;
    let (file, patch) = fixture.patch("disconnected-private-fixture.txt", "forbidden");
    mount_sse_sequence(
        &fixture.server,
        vec![patch_response("disconnected-patch", &patch)],
    )
    .await;
    fixture.supervised();
    let (mut peer, session) = fixture.peer().await;
    peer.prompt(&session, "edit then disconnect").await;
    peer.permission(&session).await;
    peer.close().await;
    assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
}
