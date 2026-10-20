use super::files::{open, tool_response};
use super::*;
use core_test_support::responses::{ev_apply_patch_custom_tool_call, ev_function_call};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn mount_cancellable(
    server: &MockServer,
    first: String,
    range: std::ops::RangeInclusive<u64>,
) {
    use wiremock::{Mock, ResponseTemplate, matchers};
    let count = Arc::new(AtomicUsize::new(0));
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/v1/responses"))
        .respond_with(move |_: &wiremock::Request| {
            let body = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                first.clone()
            } else {
                response("fresh-buffer-turn", "fresh editor turn")
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
        })
        .expect(range)
        .mount(server)
        .await;
}

pub(super) async fn wait_method(peer: &mut Peer, session: &Value, method: &str) -> Value {
    loop {
        let frame = peer.receive().await;
        assert_eq!(frame["params"]["sessionId"], *session, "{frame}");
        if frame["method"] == method {
            return frame;
        }
        assert_eq!(frame["method"], "session/update", "{frame}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_read_errors_and_oversize_responses_never_select_disk() {
    for oversized in [false, true] {
        let fixture = Fixture::new().await;
        let (file, _) = fixture.patch("read-error.txt", "ignored");
        let requests = mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_function_call(
                    "failed-read",
                    "read_file",
                    &json!({"file_path":file}).to_string(),
                )),
                response("buffer-r2", "failed read observed"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, true).await;
        let prompt = peer.prompt(&session, "read without disk fallback").await;
        let request = wait_method(&mut peer, &session, "fs/read_text_file").await;
        let mut reply = json!({"jsonrpc":"2.0","id":request["id"]});
        if oversized {
            reply["result"] = json!({"content":"x".repeat(acp_host::MAX_EDITOR_TEXT_BYTES + 1)});
        } else {
            reply["error"] = json!({"code":-32603,"message":"editor read rejected"});
        }
        peer.send(reply).await;
        peer.finish(&session, prompt, "end_turn").await;
        let body = requests.requests()[1].body_json().to_string();
        assert!(!body.contains("L1: before"), "{body}");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_editor_read_ignores_late_text_and_allows_a_fresh_turn() {
    let fixture = Fixture::new().await;
    let (file, _) = fixture.patch("read-cancel.txt", "ignored");
    mount_cancellable(
        &fixture.server,
        tool_response(ev_function_call(
            "cancelled-read",
            "read_file",
            &json!({"file_path":file}).to_string(),
        )),
        2..=3,
    )
    .await;
    let (mut peer, session) = open(&fixture, true).await;
    let prompt = peer.prompt(&session, "read then cancel").await;
    let request = wait_method(&mut peer, &session, "fs/read_text_file").await;
    peer.cancel(&session).await;
    peer.finish(&session, prompt, "cancelled").await;
    peer.send(
        json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":"late private buffer"}}),
    )
    .await;
    let fresh = peer
        .prompt(&session, "fresh prompt after cancellation")
        .await;
    let updates = peer.finish(&session, fresh, "end_turn").await;
    assert!(
        !serde_json::to_string(&updates)
            .unwrap()
            .contains("late private buffer")
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
    peer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_unacknowledged_editor_write_is_unknown_not_success_and_never_retried() {
    unknown_write(false).await;
    unknown_write(true).await;
}

async fn unknown_write(create: bool) {
    let mut fixture = Fixture::new().await;
    fixture.supervised();
    let (file, patch) = if create {
        let file = fixture.cwd.path().join("unknown-create.txt");
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+after\n*** End Patch",
            file.display()
        );
        (file, patch)
    } else {
        fixture.patch("unknown-write.txt", "after")
    };
    mount_cancellable(
        &fixture.server,
        tool_response(ev_apply_patch_custom_tool_call("unknown-edit", &patch)),
        1..=2,
    )
    .await;
    let (mut peer, session) = open(&fixture, true).await;
    let prompt = peer
        .prompt(&session, "edit then cancel before acknowledgement")
        .await;
    for stage in 0..2 {
        let read = wait_method(&mut peer, &session, "fs/read_text_file").await;
        let reply = if create {
            json!({"jsonrpc":"2.0","id":read["id"],"error":{"code":-32002,"message":"absent"}})
        } else {
            json!({"jsonrpc":"2.0","id":read["id"],"result":{"content":"before\n"}})
        };
        peer.send(reply).await;
        if stage == 0 {
            let permission = wait_method(&mut peer, &session, "session/request_permission").await;
            peer.reply_permission(&permission, "allow").await;
        }
    }
    let write = wait_method(&mut peer, &session, "fs/write_text_file").await;
    assert_eq!(write["params"]["content"], "after\n");
    peer.cancel(&session).await;
    loop {
        let frame = peer.receive().await;
        if frame["id"] == prompt {
            assert!(frame["error"].is_object(), "{frame}");
            break;
        }
        assert_eq!(frame["method"], "session/update", "{frame}");
    }
    peer.send(json!({"jsonrpc":"2.0","id":write["id"],"result":{}}))
        .await;
    let (failed, updates) = peer
        .request(
            "session/prompt",
            json!({
                "sessionId":session,"prompt":[{"type":"text","text":"do not retry"}]
            }),
        )
        .await;
    assert!(failed["error"].is_object(), "{failed}");
    assert!(updates.is_empty());
    if create {
        assert!(!file.exists());
    } else {
        assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
    }
    peer.close().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_a_patch_target_during_approval_cannot_redirect_an_editor_write() {
    let mut fixture = Fixture::new().await;
    fixture.supervised();
    let (file, patch) = fixture.patch("replaced-target.txt", "after");
    let other = TempDir::new().unwrap();
    let secret = other.path().join("secret.txt");
    std::fs::write(&secret, "outside unchanged\n").unwrap();
    mount_sse_sequence(
        &fixture.server,
        vec![
            tool_response(ev_apply_patch_custom_tool_call("replaced-edit", &patch)),
            response("buffer-r2", "replacement rejected"),
        ],
    )
    .await;
    let (mut peer, session) = open(&fixture, true).await;
    let prompt = peer.prompt(&session, "approve after target changes").await;
    let read = wait_method(&mut peer, &session, "fs/read_text_file").await;
    peer.send(json!({"jsonrpc":"2.0","id":read["id"],"result":{"content":"before\n"}}))
        .await;
    let permission = wait_method(&mut peer, &session, "session/request_permission").await;
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(&secret, &file).unwrap();
    peer.reply_permission(&permission, "allow").await;
    // finish accepts only updates/terminal response: no read or write RPC can
    // escape the changed canonical identity check.
    peer.finish(&session, prompt, "end_turn").await;
    assert_eq!(
        std::fs::read_to_string(secret).unwrap(),
        "outside unchanged\n"
    );
    peer.close().await;
}
