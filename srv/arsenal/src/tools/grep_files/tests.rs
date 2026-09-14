use super::*;

use std::os::unix::fs::symlink;

#[test]
fn search_returns_matching_files() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("match_one.txt"), "alpha beta gamma").unwrap();
    std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();
    std::fs::write(dir.join("other.txt"), "omega").unwrap();

    let results = run_grep_search("alpha", None, dir, 10).expect("search failed");
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|p| p.ends_with("match_one.txt")));
    assert!(results.iter().any(|p| p.ends_with("match_two.txt")));
}

#[test]
fn search_with_glob_filter() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("match_one.rs"), "alpha beta gamma").unwrap();
    std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();

    let results = run_grep_search("alpha", Some("*.rs"), dir, 10).expect("search failed");
    assert_eq!(results.len(), 1);
    assert!(results[0].ends_with("match_one.rs"));
}

#[test]
fn search_respects_limit() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("one.txt"), "alpha one").unwrap();
    std::fs::write(dir.join("two.txt"), "alpha two").unwrap();
    std::fs::write(dir.join("three.txt"), "alpha three").unwrap();

    let results = run_grep_search("alpha", None, dir, 2).expect("search failed");
    assert_eq!(results.len(), 2);
}

#[test]
fn search_handles_no_matches() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("one.txt"), "omega").unwrap();

    let results = run_grep_search("alpha", None, dir, 5).expect("search failed");
    assert!(results.is_empty());
}

#[test]
fn search_rejects_invalid_regex() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let err = run_grep_search("[invalid", None, temp.path(), 10).unwrap_err();
    assert!(err.contains("invalid regex"));
}

#[test]
fn search_matches_non_utf8_files() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(
        dir.join("latin1.txt"),
        [0xff, b'a', b'l', b'p', b'h', b'a', 0xfe],
    )
    .unwrap();

    let results = run_grep_search("alpha", None, dir, 10).expect("search failed");
    assert_eq!(results.len(), 1);
    assert!(results[0].ends_with("latin1.txt"));
}

#[test]
fn search_only_reads_regular_files() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    let real = dir.join("match.txt");
    let alias = dir.join("match-link.txt");
    std::fs::write(&real, "alpha beta gamma").unwrap();
    symlink(&real, &alias).expect("create symlink");

    let results = run_grep_search("alpha", None, dir, 10).expect("search failed");
    assert_eq!(results.len(), 1);
    assert!(results[0].ends_with("match.txt"));
}

#[tokio::test]
async fn rejects_unknown_arguments() {
    let result = execute(&serde_json::json!({
        "pattern": "alpha",
        "pathh": "."
    }))
    .await;

    let err = result.expect_err("unknown field should fail");
    assert!(err.contains("unknown field `pathh`"));
}
