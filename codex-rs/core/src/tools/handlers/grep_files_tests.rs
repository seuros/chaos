use chaos_arsenal::tools::grep_files::{parse_results, run_rg_search};
use std::process::Command as StdCommand;

#[test]
fn parses_basic_results() {
    let stdout = b"/tmp/file_a.rs\n/tmp/file_b.rs\n";
    let parsed = parse_results(stdout, 10);
    assert_eq!(
        parsed,
        vec!["/tmp/file_a.rs".to_string(), "/tmp/file_b.rs".to_string()]
    );
}

#[test]
fn parse_truncates_after_limit() {
    let stdout = b"/tmp/file_a.rs\n/tmp/file_b.rs\n/tmp/file_c.rs\n";
    let parsed = parse_results(stdout, 2);
    assert_eq!(
        parsed,
        vec!["/tmp/file_a.rs".to_string(), "/tmp/file_b.rs".to_string()]
    );
}

#[tokio::test]
async fn run_search_returns_results() {
    if !rg_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("match_one.txt"), "alpha beta gamma").unwrap();
    std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();
    std::fs::write(dir.join("other.txt"), "omega").unwrap();

    let results = run_rg_search("alpha", None, dir, 10)
        .await
        .expect("search failed");
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|path| path.ends_with("match_one.txt")));
    assert!(results.iter().any(|path| path.ends_with("match_two.txt")));
}

#[tokio::test]
async fn run_search_with_glob_filter() {
    if !rg_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("match_one.rs"), "alpha beta gamma").unwrap();
    std::fs::write(dir.join("match_two.txt"), "alpha delta").unwrap();

    let results = run_rg_search("alpha", Some("*.rs"), dir, 10)
        .await
        .expect("search failed");
    assert_eq!(results.len(), 1);
    assert!(results.iter().all(|path| path.ends_with("match_one.rs")));
}

#[tokio::test]
async fn run_search_respects_limit() {
    if !rg_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("one.txt"), "alpha one").unwrap();
    std::fs::write(dir.join("two.txt"), "alpha two").unwrap();
    std::fs::write(dir.join("three.txt"), "alpha three").unwrap();

    let results = run_rg_search("alpha", None, dir, 2)
        .await
        .expect("search failed");
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn run_search_handles_no_matches() {
    if !rg_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("one.txt"), "omega").unwrap();

    let results = run_rg_search("alpha", None, dir, 5)
        .await
        .expect("search failed");
    assert!(results.is_empty());
}

fn rg_available() -> bool {
    StdCommand::new("rg")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
