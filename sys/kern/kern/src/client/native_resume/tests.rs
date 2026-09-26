use super::*;

const ID: &str = "2eb242f5-85ad-4407-b9d3-459c32d06d91";

fn checkpoint(backend: Backend) -> Checkpoint {
    Checkpoint::completed(
        backend,
        ID,
        "claude",
        "system",
        "/work".into(),
        vec!["first".into()],
        "answer",
    )
    .unwrap()
}

fn next_input() -> Vec<String> {
    vec![
        "first".into(),
        "<message role=\"assistant\">\nanswer\n</message>".into(),
        "<message role=\"developer\">\nnew hook\n</message>".into(),
        "<message role=\"user\">\nsecond\n</message>".into(),
    ]
}

#[test]
fn native_resume_requires_completed_matching_history_and_sends_all_new_items() {
    for backend in [Backend::Claude, Backend::Antigravity] {
        let cp = checkpoint(backend);
        let input = next_input();
        assert_eq!(
            cp.continuation(
                backend,
                "claude",
                "system",
                std::path::Path::new("/work"),
                &input
            ),
            Some((ID.into(), input[2..].join("\n\n")))
        );
        for (model, system, cwd) in [
            ("other", "system", "/work"),
            ("claude", "changed", "/work"),
            ("claude", "system", "/other"),
        ] {
            assert!(
                cp.continuation(backend, model, system, std::path::Path::new(cwd), &input)
                    .is_none()
            );
        }
        for input in [
            vec![],
            vec!["first".into()],
            input[..2].to_vec(),
            vec!["compacted".into(), input[1].clone(), "second".into()],
            vec!["first".into(), "different answer".into(), "second".into()],
        ] {
            assert!(
                cp.continuation(
                    backend,
                    "claude",
                    "system",
                    std::path::Path::new("/work"),
                    &input
                )
                .is_none()
            );
        }
    }
}

#[test]
fn checkpoint_is_private_consumed_before_dispatch_and_scoped_to_one_process() {
    for backend in [Backend::Claude, Backend::Antigravity] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("clamp/claude/process-a.json");
        let first = NativeResume::new(Some(path.clone()));
        first.save(checkpoint(backend)).unwrap();
        drop(first);
        let resumed = NativeResume::new(Some(path.clone()));
        let other = NativeResume::new(Some(home.path().join("clamp/claude/process-b.json")));
        assert!(other.take().unwrap().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let cp = resumed.take().unwrap().unwrap();
        assert!(
            cp.continuation(
                backend,
                "claude",
                "system",
                std::path::Path::new("/work"),
                &next_input()
            )
            .is_some()
        );
        assert!(!path.exists());
        assert!(
            NativeResume::new(Some(path.clone()))
                .take()
                .unwrap()
                .is_none(),
            "interrupted dispatch must not resume an advanced provider transcript"
        );
        resumed.save(checkpoint(backend)).unwrap();
        resumed.clear().unwrap();
        assert!(resumed.take().unwrap().is_none());
        std::fs::write(&path, "invalid json").unwrap();
        assert!(resumed.take().unwrap().is_none());
        assert!(!path.exists());
    }
}

#[test]
fn ephemeral_checkpoint_stays_in_memory_and_invalid_ids_are_not_resumable() {
    for backend in [Backend::Claude, Backend::Antigravity] {
        let store = NativeResume::new(None);
        store.save(checkpoint(backend)).unwrap();
        assert!(store.take().unwrap().is_some());
        assert!(store.take().unwrap().is_none());
        assert!(
            Checkpoint::completed(
                backend,
                "invalid/id",
                "claude",
                "system",
                "/work".into(),
                vec![],
                ""
            )
            .is_none()
        );
        let mut cp = checkpoint(backend);
        cp.session_id = "invalid/id".into();
        assert!(
            cp.continuation(
                backend,
                "claude",
                "system",
                std::path::Path::new("/work"),
                &next_input()
            )
            .is_none()
        );
        cp.session_id = ID.into();
        cp.version = 99;
        assert!(
            cp.continuation(
                backend,
                "claude",
                "system",
                std::path::Path::new("/work"),
                &next_input()
            )
            .is_none()
        );
    }
}

#[test]
fn backend_ids_and_legacy_checkpoint_compatibility() {
    assert!(Backend::Antigravity.valid_id("native_id-7"));
    assert!(!Backend::Claude.valid_id("native_id-7"));
    for id in ["", "a/b", "a b", "a\n", &"a".repeat(257)] {
        assert!(!Backend::Antigravity.valid_id(id));
    }
    let cp = checkpoint(Backend::Claude);
    assert!(
        cp.continuation(
            Backend::Antigravity,
            "claude",
            "system",
            std::path::Path::new("/work"),
            &next_input()
        )
        .is_none()
    );
    let mut legacy = serde_json::to_value(&cp).unwrap();
    legacy.as_object_mut().unwrap().remove("backend");
    let legacy: Checkpoint = serde_json::from_value(legacy).unwrap();
    assert!(
        legacy
            .continuation(
                Backend::Claude,
                "claude",
                "system",
                std::path::Path::new("/work"),
                &next_input()
            )
            .is_some()
    );
    let old_agy = serde_json::json!({"version":1,"model":"gemini","conversation_id":"native_id"});
    assert!(serde_json::from_value::<Checkpoint>(old_agy).is_err());
}

#[test]
fn rendered_delta_delivers_new_hook_and_system_items_without_replaying_consumed_notifications() {
    use chaos_ipc::models::{ContentItem, ResponseItem};
    let message = |role: &str, text: &str| ResponseItem::Message {
        id: None,
        role: role.into(),
        content: vec![ContentItem::InputText { text: text.into() }],
        end_turn: None,
        phase: None,
    };
    for backend in [Backend::Claude, Backend::Antigravity] {
        let mut prompt = Prompt::default();
        prompt.base_instructions.text = "Canonical system instructions".into();
        prompt.input = vec![
            message("user", "first"),
            message("system", "<mcp_resource_update>old</mcp_resource_update>"),
        ];
        let first = rendered_input(&prompt);
        assert!(
            first
                .iter()
                .all(|item| !item.contains("Canonical system instructions"))
        );
        let cp =
            Checkpoint::completed(backend, ID, "system", "/work".into(), first, "answer").unwrap();
        prompt.input.extend([
            message("assistant", "answer"),
            message("developer", "Stop hook: continue once"),
            message("system", "<mcp_resource_update>new</mcp_resource_update>"),
            message("system", "new canonical system notice"),
        ]);
        let (_, delta) = cp
            .continuation(
                backend,
                "system",
                std::path::Path::new("/work"),
                &rendered_input(&prompt),
            )
            .unwrap();
        assert!(delta.contains("role=\"developer\""));
        assert!(delta.contains("Stop hook: continue once"));
        assert!(delta.contains("<mcp_resource_update>new</mcp_resource_update>"));
        assert!(delta.contains("new canonical system notice"));
        assert!(!delta.contains("<mcp_resource_update>old"));
        assert!(!delta.contains("first"));
        // No new user message is needed, and canonical roles remain intact.
        assert!(
            matches!(&prompt.input[3], ResponseItem::Message { role, .. } if role == "developer")
        );
    }
}
