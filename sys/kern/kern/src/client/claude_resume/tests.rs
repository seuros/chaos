use super::*;

const ID: &str = "2eb242f5-85ad-4407-b9d3-459c32d06d91";

fn checkpoint() -> Checkpoint {
    Checkpoint::completed(
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
    let cp = checkpoint();
    let input = next_input();
    assert_eq!(
        cp.continuation("claude", "system", std::path::Path::new("/work"), &input),
        Some((ID.into(), input[2..].join("\n\n")))
    );
    for (model, system, cwd) in [
        ("other", "system", "/work"),
        ("claude", "changed", "/work"),
        ("claude", "system", "/other"),
    ] {
        assert!(
            cp.continuation(model, system, std::path::Path::new(cwd), &input)
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
            cp.continuation("claude", "system", std::path::Path::new("/work"), &input)
                .is_none()
        );
    }
}

#[test]
fn checkpoint_is_private_consumed_before_dispatch_and_scoped_to_one_process() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("clamp/claude/process-a.json");
    let first = ClaudeResume::new(Some(path.clone()));
    first.save(checkpoint()).unwrap();
    drop(first);
    let resumed = ClaudeResume::new(Some(path.clone()));
    let other = ClaudeResume::new(Some(home.path().join("clamp/claude/process-b.json")));
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
            "claude",
            "system",
            std::path::Path::new("/work"),
            &next_input()
        )
        .is_some()
    );
    assert!(!path.exists());
    assert!(
        ClaudeResume::new(Some(path.clone()))
            .take()
            .unwrap()
            .is_none(),
        "interrupted dispatch must not resume an advanced provider transcript"
    );
    resumed.save(checkpoint()).unwrap();
    resumed.clear().unwrap();
    assert!(resumed.take().unwrap().is_none());
    std::fs::write(&path, "invalid json").unwrap();
    assert!(resumed.take().unwrap().is_none());
    assert!(!path.exists());
}

#[test]
fn ephemeral_checkpoint_stays_in_memory_and_invalid_ids_are_not_resumable() {
    let store = ClaudeResume::new(None);
    store.save(checkpoint()).unwrap();
    assert!(store.take().unwrap().is_some());
    assert!(store.take().unwrap().is_none());
    assert!(
        Checkpoint::completed("default", "claude", "system", "/work".into(), vec![], "").is_none()
    );
    let mut cp = checkpoint();
    cp.session_id = "--continue".into();
    assert!(
        cp.continuation(
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
            "claude",
            "system",
            std::path::Path::new("/work"),
            &next_input()
        )
        .is_none()
    );
}
