use super::file_faults::wait_method;
use super::files::{open, tool_response};
use super::*;
use core_test_support::responses::ev_apply_patch_custom_tool_call;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_creation_requires_absence_approval_and_confirmed_write() {
    for scenario in [
        "allow",
        "deny",
        "readonly",
        "editor-exists",
        "editor-race",
        "disk-race",
        "read-error",
    ] {
        let mut fixture = Fixture::new().await;
        fixture.supervised();
        let file = fixture.cwd.path().join("new.txt");
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+new editor text\n*** End Patch",
            file.display()
        );
        let requests = mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_apply_patch_custom_tool_call("create", &patch)),
                response("created", "creation handled"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, scenario != "readonly").await;
        let prompt = peer.prompt(&session, "create a file").await;
        let (mut reads, mut writes, mut approvals) = (0, 0, 0);
        loop {
            let frame = peer.receive().await;
            if frame["id"] == prompt {
                assert_eq!(
                    frame["result"]["stopReason"], "end_turn",
                    "{scenario}: {frame}"
                );
                break;
            }
            assert_eq!(frame["params"]["sessionId"], session, "{frame}");
            match frame["method"].as_str() {
                Some("fs/read_text_file") => {
                    reads += 1;
                    assert_eq!(
                        frame["params"]["path"],
                        std::fs::canonicalize(fixture.cwd.path())
                            .unwrap()
                            .join("new.txt")
                            .to_str()
                            .unwrap()
                    );
                    if scenario == "editor-exists" || (scenario == "editor-race" && reads == 2) {
                        peer.send(
                            json!({"jsonrpc":"2.0","id":frame["id"],"result":{"content":""}}),
                        )
                        .await;
                    } else {
                        let code = if scenario == "read-error" {
                            -32603
                        } else {
                            -32002
                        };
                        peer.send(json!({
                            "jsonrpc":"2.0", "id":frame["id"],
                            "error":{"code":code,"message":"read failed"}
                        }))
                        .await;
                    }
                }
                Some("session/request_permission") => {
                    approvals += 1;
                    assert_eq!(frame["params"]["toolCall"]["kind"], "edit");
                    if scenario == "disk-race" {
                        std::fs::write(&file, "concurrent disk file\n").unwrap();
                    }
                    peer.reply_permission(
                        &frame,
                        if scenario == "deny" { "deny" } else { "allow" },
                    )
                    .await;
                }
                Some("fs/write_text_file") => {
                    writes += 1;
                    assert_eq!(scenario, "allow");
                    assert_eq!(frame["params"]["content"], "new editor text\n");
                    peer.send(json!({"jsonrpc":"2.0","id":frame["id"],"result":{}}))
                        .await;
                }
                Some("session/update") => {}
                _ => panic!("unexpected frame: {frame}"),
            }
        }
        let expected = match scenario {
            "allow" => (2, 1, 1),
            "deny" | "disk-race" => (1, 0, 1),
            "readonly" => (0, 0, 0),
            "editor-race" => (2, 0, 1),
            _ => (1, 0, 0),
        };
        assert_eq!((reads, writes, approvals), expected, "{scenario}");
        if scenario == "disk-race" {
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "concurrent disk file\n"
            );
        } else {
            assert!(
                !file.exists(),
                "editor creation must never write native disk"
            );
        }
        if scenario == "allow" {
            assert!(
                requests.requests()[1]
                    .body_json()
                    .to_string()
                    .contains("A ")
            );
        }
        peer.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_rejects_existing_disk_files_and_missing_parents_before_editor_io() {
    for existing in [false, true] {
        let fixture = Fixture::new().await;
        let file = if existing {
            let file = fixture.cwd.path().join("existing.txt");
            std::fs::write(&file, "do not replace\n").unwrap();
            file
        } else {
            fixture.cwd.path().join("missing-parent/new.txt")
        };
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+new\n*** End Patch",
            file.display()
        );
        mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_apply_patch_custom_tool_call("create", &patch)),
                response("rejected", "rejected"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, true).await;
        let prompt = peer.prompt(&session, "create").await;
        peer.finish(&session, prompt, "end_turn").await; // Panics on any editor request.
        if existing {
            assert_eq!(std::fs::read_to_string(file).unwrap(), "do not replace\n");
        } else {
            assert!(!file.exists());
        }
        peer.close().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_cannot_be_redirected_by_parent_replacement_after_approval() {
    let mut fixture = Fixture::new().await;
    fixture.supervised();
    let parent = fixture.cwd.path().join("parent");
    let replacement = fixture.cwd.path().join("replacement");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    let file = parent.join("new.txt");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+created\n*** End Patch",
        file.display()
    );
    mount_sse_sequence(
        &fixture.server,
        vec![
            tool_response(ev_apply_patch_custom_tool_call("parent-race", &patch)),
            response("rejected", "parent replacement rejected"),
        ],
    )
    .await;
    let (mut peer, session) = open(&fixture, true).await;
    let prompt = peer.prompt(&session, "create after approval").await;
    let read = wait_method(&mut peer, &session, "fs/read_text_file").await;
    peer.send(json!({"jsonrpc":"2.0","id":read["id"],"error":{"code":-32002,"message":"absent"}}))
        .await;
    let permission = wait_method(&mut peer, &session, "session/request_permission").await;
    std::fs::rename(&parent, fixture.cwd.path().join("original-parent")).unwrap();
    std::os::unix::fs::symlink(&replacement, &parent).unwrap();
    peer.reply_permission(&permission, "allow").await;
    peer.finish(&session, prompt, "end_turn").await;
    assert!(!file.exists());
    assert!(!replacement.join("new.txt").exists());
    peer.close().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creation_rejects_outside_and_symlink_targets_before_editor_requests() {
    use chaos_ipc::protocol::{VfsAccessMode, VfsEntry, VfsPath, VfsPolicy, VfsSpecialPath};
    for target in ["outside", "symlink-parent", "dangling-leaf"] {
        let mut fixture = Fixture::new().await;
        fixture.config.permissions.vfs_policy = VfsPolicy::restricted(vec![VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::CurrentWorkingDirectory,
            },
            access: VfsAccessMode::Write,
        }]);
        let outside = TempDir::new().unwrap();
        let file = match target {
            "outside" => outside.path().join("new.txt"),
            "symlink-parent" => {
                let link = fixture.cwd.path().join("link");
                std::os::unix::fs::symlink(outside.path(), &link).unwrap();
                link.join("new.txt")
            }
            _ => {
                let file = fixture.cwd.path().join("dangling.txt");
                std::os::unix::fs::symlink(outside.path().join("absent.txt"), &file).unwrap();
                file
            }
        };
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+new\n*** End Patch",
            file.display()
        );
        mount_sse_sequence(
            &fixture.server,
            vec![
                tool_response(ev_apply_patch_custom_tool_call("unsafe-create", &patch)),
                response("rejected", "creation rejected"),
            ],
        )
        .await;
        let (mut peer, session) = open(&fixture, true).await;
        let prompt = peer.prompt(&session, "try creating").await;
        peer.finish(&session, prompt, "end_turn").await;
        assert!(!file.exists());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        peer.close().await;
    }
}
