use super::*;
use core_test_support::responses::{ev_apply_patch_custom_tool_call, ev_function_call};

pub(super) async fn open(fixture: &Fixture, write: bool) -> (Peer, Value) {
    let mut peer = Peer::connect_capabilities(
        fixture.config.clone(),
        json!({
            "fs":{"readTextFile":true,"writeTextFile":write}
        }),
    )
    .await;
    let (opened, _) = peer
        .request(
            "session/new",
            json!({
                "cwd":fixture.cwd.path(),"mcpServers":[]
            }),
        )
        .await;
    let id = opened["result"]["sessionId"].clone();
    assert!(id.is_string(), "{opened}");
    (peer, id)
}

#[derive(Default)]
struct Editor {
    text: String,
    reads: usize,
    writes: usize,
    approvals: usize,
    allow: bool,
    conflict: bool,
}

impl Editor {
    async fn finish(&mut self, peer: &mut Peer, session: &Value, prompt: u64) -> Vec<Value> {
        let mut updates = Vec::new();
        loop {
            let frame = peer.receive().await;
            if frame["id"] == prompt {
                assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                break;
            }
            assert_eq!(frame["params"]["sessionId"], *session, "{frame}");
            match frame["method"].as_str() {
                Some("fs/read_text_file") => {
                    self.reads += 1;
                    assert!(
                        std::path::Path::new(frame["params"]["path"].as_str().unwrap())
                            .is_absolute()
                    );
                    if self.conflict && self.reads > 1 {
                        self.text = "concurrent editor typing\n".into();
                    }
                    peer.send(
                        json!({"jsonrpc":"2.0","id":frame["id"],"result":{"content":self.text}}),
                    )
                    .await;
                }
                Some("fs/write_text_file") => {
                    self.writes += 1;
                    self.text = frame["params"]["content"].as_str().unwrap().into();
                    peer.send(json!({"jsonrpc":"2.0","id":frame["id"],"result":{}}))
                        .await;
                }
                Some("session/request_permission") => {
                    self.approvals += 1;
                    peer.reply_permission(&frame, if self.allow { "allow" } else { "deny" })
                        .await;
                }
                Some("session/update") => updates.push(frame),
                _ => panic!("unexpected editor frame {frame}"),
            }
        }
        updates
    }
}

pub(super) fn tool_response(call: Value) -> String {
    sse(vec![
        ev_response_created("buffer-r1"),
        call,
        ev_completed("buffer-r1"),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_reads_use_unsaved_editor_text_with_shared_slice_and_indentation_formatting() {
    for mode in ["slice", "indentation"] {
        let fixture = Fixture::new().await;
        let file = fixture.cwd.path().join("buffer.rs");
        std::fs::write(&file, "disk-only-marker\n").unwrap();
        let requests = mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_function_call(
                    "buffer-read",
                    "read_file",
                    &json!({
                        "file_path":file,"offset":2,"limit":1,"mode":mode
                    })
                    .to_string(),
                )),
                response("buffer-r2", "read complete"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, false).await;
        let prompt = peer.prompt(&session, "read the unsaved file").await;
        let mut editor = Editor {
            text: "fn main() {\n    unsaved_marker();\n}\n".into(),
            ..Default::default()
        };
        editor.finish(&mut peer, &session, prompt).await;
        assert_eq!((editor.reads, editor.writes), (1, 0));
        let body = requests.requests()[1].body_json().to_string();
        assert!(body.contains("L2:     unsaved_marker();"), "{body}");
        assert!(!body.contains("disk-only-marker"));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "disk-only-marker\n");
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_editor_edits_preserve_approval_mode_read_only_and_conflict_ceilings() {
    for scenario in ["allow", "deny", "conflict", "read-only", "plan"] {
        let mut fixture = Fixture::new().await;
        fixture.supervised();
        let (file, _) = fixture.patch("buffer.txt", "ignored");
        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n-buffer-before\n+buffer-after\n*** End Patch",
            file.display()
        );
        mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_apply_patch_custom_tool_call("buffer-edit", &patch)),
                response("buffer-r2", "edit complete"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, scenario != "read-only").await;
        if scenario == "plan" {
            let (mode, _) = peer
                .request(
                    "session/set_mode",
                    json!({"sessionId":session,"modeId":"plan"}),
                )
                .await;
            assert!(mode["error"].is_null(), "{mode}");
        }
        let prompt = peer.prompt(&session, "edit the unsaved buffer").await;
        let mut editor = Editor {
            text: "buffer-before\n".into(),
            allow: scenario != "deny",
            conflict: scenario == "conflict",
            ..Default::default()
        };
        let updates = editor.finish(&mut peer, &session, prompt).await;
        let expected = match scenario {
            "allow" => (2, 1, 1),
            "deny" => (1, 0, 1),
            "conflict" => (2, 0, 1),
            _ => (0, 0, 0),
        };
        assert_eq!(
            (editor.reads, editor.writes, editor.approvals),
            expected,
            "{scenario}: {updates:#?}"
        );
        assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
        if scenario == "allow" {
            assert_eq!(editor.text, "buffer-after\n");
        }
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_editor_patch_shapes_fail_before_editor_requests_or_disk_effects() {
    for shape in ["create", "delete", "rename", "multiple"] {
        let fixture = Fixture::new().await;
        let (file, _) = fixture.patch("buffer.txt", "ignored");
        let other = fixture.cwd.path().join("other.txt");
        let body = match shape {
            "create" => format!("*** Add File: {}\n+forbidden", other.display()),
            "delete" => format!("*** Delete File: {}", file.display()),
            "rename" => format!(
                "*** Update File: {}\n*** Move to: {}\n-before\n+after",
                file.display(),
                other.display()
            ),
            _ => format!(
                "*** Update File: {}\n-before\n+after\n*** Add File: {}\n+forbidden",
                file.display(),
                other.display()
            ),
        };
        let patch = format!("*** Begin Patch\n{body}\n*** End Patch");
        mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_apply_patch_custom_tool_call("unsupported-edit", &patch)),
                response("buffer-r2", "unsupported edit rejected"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, true).await;
        let prompt = peer.prompt(&session, "try the unsupported edit").await;
        let mut editor = Editor {
            text: "before\n".into(),
            allow: true,
            ..Default::default()
        };
        editor.finish(&mut peer, &session, prompt).await;
        assert_eq!(
            (editor.reads, editor.writes, editor.approvals),
            (0, 0, 0),
            "{shape}"
        );
        assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
        assert!(!other.exists());
        peer.close().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_reads_cannot_escape_native_vfs_through_paths_or_symlinks() {
    use chaos_ipc::protocol::{VfsAccessMode, VfsEntry, VfsPath, VfsPolicy, VfsSpecialPath};
    for symlink in [false, true] {
        let mut fixture = Fixture::new().await;
        fixture.config.permissions.vfs_policy = VfsPolicy::restricted(vec![VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::CurrentWorkingDirectory,
            },
            access: VfsAccessMode::Read,
        }]);
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "outside disk marker\n").unwrap();
        let file = if symlink {
            let link = fixture.cwd.path().join("link.txt");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            link
        } else {
            secret.clone()
        };
        mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_function_call(
                    "forbidden-read",
                    "read_file",
                    &json!({"file_path":file}).to_string(),
                )),
                response("buffer-r2", "denied"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, true).await;
        let prompt = peer.prompt(&session, "try reading outside").await;
        let mut editor = Editor {
            text: "outside editor marker\n".into(),
            ..Default::default()
        };
        editor.finish(&mut peer, &session, prompt).await;
        assert_eq!((editor.reads, editor.writes), (0, 0));
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_sessions_keep_their_editor_text_and_model_contexts_isolated() {
    let fixture = Fixture::new().await;
    let (file, _) = fixture.patch("isolated-buffer.txt", "ignored");
    let args = json!({"file_path":file}).to_string();
    let requests = mount_sse_sequence(
        &fixture.server,
        vec![
            tool_response(ev_function_call("first-read", "read_file", &args)),
            response("first-done", "first complete"),
            tool_response(ev_function_call("second-read", "read_file", &args)),
            response("second-done", "second complete"),
        ],
    )
    .await;
    let (mut peer, first) = open(&fixture, true).await;
    let (opened, _) = peer
        .request(
            "session/new",
            json!({
                "cwd":fixture.cwd.path(),"mcpServers":[]
            }),
        )
        .await;
    let second = opened["result"]["sessionId"].clone();
    assert_ne!(first, second);
    for (session, text) in [
        (&first, "first private editor text"),
        (&second, "second private editor text"),
    ] {
        let prompt = peer
            .prompt(session, "read this session's editor text")
            .await;
        let mut editor = Editor {
            text: text.into(),
            ..Default::default()
        };
        editor.finish(&mut peer, session, prompt).await;
        assert_eq!((editor.reads, editor.writes), (1, 0));
    }
    let body = requests.requests()[3].body_json().to_string();
    assert!(body.contains("second private editor text"));
    assert!(!body.contains("first private editor text"));
    assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
    peer.close().await;
}
