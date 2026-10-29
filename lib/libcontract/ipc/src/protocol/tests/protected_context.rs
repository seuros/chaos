use std::path::PathBuf;

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
    let (snapshot, warnings) = ProtectedContext::read(&paths, dir.path(), 1024);
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
    let rendered = snapshot.expect("snapshot").render();
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
    let (snapshot, warnings) = ProtectedContext::read(&paths, dir.path(), 7);
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
    assert!(
        snapshot
            .render()
            .contains("truncated at the configured byte cap")
    );
}

#[test]
fn unreadable_files_are_warned_and_skipped_but_the_snapshot_is_still_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("binary.bin"), [0xff, 0xfe, 0x00]).expect("write");
    let paths = vec![PathBuf::from("absent.md"), PathBuf::from("binary.bin")];
    let (snapshot, warnings) = ProtectedContext::read(&paths, dir.path(), 1024);
    let snapshot = snapshot.expect("an attempted snapshot is recorded even when empty");
    assert!(snapshot.is_empty());
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("absent.md"));
    assert!(warnings[1].contains("not UTF-8"));

    let (none, warnings) = ProtectedContext::read(&[], dir.path(), 1024);
    assert_eq!((none, warnings), (None, Vec::new()));
}

#[test]
fn meta_round_trips_the_snapshot_and_old_meta_reads_without_one() {
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
    assert!(
        !json.contains("truncated"),
        "false is not serialized: {json}"
    );
    let item: RolloutItem = serde_json::from_str(&json).expect("deserialize");
    let history = InitialHistory::Forked(vec![item]);
    assert_eq!(history.get_protected_context(), Some(snapshot));

    let old = serde_json::to_value(RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta::default(),
        git: None,
    }))
    .expect("serialize old meta");
    assert!(old.to_string().find("protected_context").is_none());
    let item: RolloutItem = serde_json::from_value(old).expect("old meta still reads");
    assert_eq!(
        InitialHistory::Forked(vec![item]).get_protected_context(),
        None
    );
}
