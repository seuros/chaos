use super::*;

use pretty_assertions::assert_eq;

use crate::protocol::InitialHistory;
use crate::protocol::ProtectedContext;
use crate::protocol::ProtectedContextFile;
use crate::protocol::RolloutItem;
use crate::protocol::SessionMeta;
use crate::protocol::SessionMetaLine;

fn files(dir: &tempfile::TempDir, contents: &[(&str, &str)]) -> Vec<PathBuf> {
    contents
        .iter()
        .map(|(name, text)| {
            std::fs::write(dir.path().join(name), text).expect("write protected file");
            PathBuf::from(name)
        })
        .collect()
}

#[test]
fn read_resolves_relative_paths_against_cwd_and_keeps_them_relative() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = files(&dir, &[("soul.md", "soul\n"), ("story.md", "story")]);
    let (snapshot, warnings) = read(&paths, dir.path(), 1024);
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(
        snapshot,
        Some(ProtectedContext {
            files: vec![
                ProtectedContextFile {
                    path: "soul.md".to_string(),
                    text: "soul\n".to_string(),
                },
                ProtectedContextFile {
                    path: "story.md".to_string(),
                    text: "story".to_string(),
                },
            ],
            truncated: false,
        })
    );
    let rendered = render(&snapshot.expect("snapshot"));
    assert!(rendered.starts_with("<protected_context>"));
    assert!(rendered.contains("<file path=\"soul.md\">\nsoul\n</file>\n"));
    assert!(rendered.contains("<file path=\"story.md\">\nstory\n</file>\n"));
    assert!(!rendered.contains("truncated"));
}

#[test]
fn the_cap_is_shared_across_files_and_cuts_at_a_character_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    // "é" is two bytes; a 6-byte allowance would split the second one.
    let paths = files(
        &dir,
        &[("a.md", "abcd"), ("b.md", "éé"), ("c.md", "never read")],
    );
    let (snapshot, warnings) = read(&paths, dir.path(), 7);
    let snapshot = snapshot.expect("snapshot");
    assert!(snapshot.truncated);
    let texts = snapshot
        .files
        .iter()
        .map(|file| file.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(texts, vec!["abcd", "é"]);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("b.md") && warnings[0].contains("incomplete"));
    assert!(warnings[1].contains("c.md") && warnings[1].contains("left out"));
    assert!(render(&snapshot).contains("truncated at the configured byte cap"));
}

#[test]
fn unreadable_files_are_warned_and_skipped_but_the_snapshot_is_still_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("binary.bin"), [0xff, 0xfe, 0x00]).expect("write");
    let paths = vec![PathBuf::from("absent.md"), PathBuf::from("binary.bin")];
    let (snapshot, warnings) = read(&paths, dir.path(), 1024);
    let snapshot = snapshot.expect("an attempted snapshot is recorded even when empty");
    assert!(snapshot.is_empty());
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("absent.md"));
    assert!(warnings[1].contains("not UTF-8"));

    let (none, warnings) = read(&[], dir.path(), 1024);
    assert_eq!((none, warnings), (None, Vec::new()));
}

#[test]
fn meta_round_trips_the_snapshot() {
    let snapshot = ProtectedContext {
        files: vec![ProtectedContextFile {
            path: "/abs/soul.md".to_string(),
            text: "soul".to_string(),
        }],
        truncated: false,
    };
    let meta = SessionMeta {
        protected_context: Some(snapshot.clone()),
        ..SessionMeta::default()
    };
    let line = SessionMetaLine { meta, git: None };
    let json = serde_json::to_string(&RolloutItem::SessionMeta(line)).expect("serialize");
    let item: RolloutItem = serde_json::from_str(&json).expect("deserialize");
    let history = InitialHistory::Forked(vec![item]);
    assert_eq!(history.get_protected_context(), Some(snapshot));
}

#[test]
fn repeated_forks_keep_one_snapshot_and_preserve_other_instructions() {
    use chaos_ipc::models::DeveloperInstructions;
    use chaos_ipc::protocol::CompactedItem;

    let snapshot = ProtectedContext {
        files: vec![ProtectedContextFile {
            path: "soul.md".to_string(),
            text: "identity".to_string(),
        }],
        truncated: false,
    };
    let protected: ResponseItem = DeveloperInstructions::new(render(&snapshot)).into();
    let other: ResponseItem = DeveloperInstructions::new("other instructions").into();
    let mut items = vec![
        RolloutItem::ResponseItem(protected.clone()),
        RolloutItem::ResponseItem(other.clone()),
        RolloutItem::Compacted(CompactedItem {
            message: "summary".to_string(),
            replacement_history: vec![protected.clone(), other.clone()],
        }),
    ];
    for _ in 0..3 {
        remove_inherited(&mut items, &snapshot);
        assert_eq!(items.len(), 2);
        let RolloutItem::ResponseItem(retained) = &items[0] else {
            panic!("other instructions must remain");
        };
        assert_eq!(retained, &other);
        let RolloutItem::Compacted(checkpoint) = &items[1] else {
            panic!("checkpoint must remain");
        };
        assert_eq!(checkpoint.replacement_history, vec![other.clone()]);
        // The fork's fresh initial context adds back exactly one copy.
        items.push(RolloutItem::ResponseItem(protected.clone()));
        assert_eq!(items.len(), 3);
    }
}

#[cfg(unix)]
#[test]
fn non_regular_files_are_skipped_without_blocking_and_symlinks_are_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join("pipe");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
    // SAFETY: c_path is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
    std::fs::write(dir.path().join("real.md"), "real text").expect("write");
    std::os::unix::fs::symlink(dir.path().join("real.md"), dir.path().join("link.md"))
        .expect("symlink");

    // A FIFO with no writer would block a plain open forever; run the read
    // on a thread so a regression fails the test instead of hanging it.
    let cwd = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let paths = vec![PathBuf::from("pipe"), PathBuf::from("link.md")];
        tx.send(read(&paths, &cwd, 1024)).expect("send read result");
    });
    let (snapshot, warnings) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("reading a FIFO must not block");
    let snapshot = snapshot.expect("snapshot recorded");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("pipe") && warnings[0].contains("not a regular file"));
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files[0].path, "link.md");
    assert_eq!(snapshot.files[0].text, "real text");
}
