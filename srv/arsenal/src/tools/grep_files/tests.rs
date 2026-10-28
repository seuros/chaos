use super::*;

use std::fs::File;
use std::os::unix::fs::symlink;

#[path = "tests/baseline.rs"]
mod baseline;

#[test]
fn search_accepts_a_single_file_and_returns_an_absolute_path() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("one.rs");
    std::fs::write(&file, "alpha\n").unwrap();
    assert_eq!(
        run_grep_search("alpha", Some("*.rs"), &file, 10, None).unwrap(),
        vec![file.to_string_lossy().into_owned()]
    );

    let results =
        run_grep_search("chaos-arsenal", None, Path::new("Cargo.toml"), 10, None).unwrap();
    assert_eq!(results.len(), 1);
    assert!(Path::new(&results[0]).is_absolute());
}

#[test]
fn search_preserves_raw_regexes_and_multiline_matches() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("one.txt");
    std::fs::write(&file, "prefix\nalpha != beta\ngamma\n").unwrap();
    for pattern in [
        "^alpha != beta$",
        r"alpha != beta\ngamma",
        "(?s)alpha.*gamma",
        r"\w+ != \w+",
    ] {
        let results = run_grep_search(pattern, None, temp.path(), 10, Some(true)).unwrap();
        assert_eq!(results.len(), 1, "{pattern}");
    }
}

#[test]
fn search_selects_newest_matches_before_limiting_and_breaks_ties_by_path() {
    let temp = tempfile::tempdir().unwrap();
    let base_time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    for (name, seconds) in [("old.txt", 0), ("new-b.txt", 100), ("new-a.txt", 100)] {
        let path = temp.path().join(name);
        std::fs::write(&path, "alpha").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(base_time + std::time::Duration::from_secs(seconds)),
            )
            .unwrap();
    }
    let results = run_grep_search("alpha", None, temp.path(), 1, None).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].ends_with("new-a.txt"));
}

#[tokio::test]
async fn search_case_mode_is_optional_and_can_override_smart_case() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("lower.txt"), "alpha").unwrap();
    std::fs::write(dir.join("upper.txt"), "ALPHA").unwrap();

    for (pattern, case_sensitive, expected) in [
        ("alpha", None, vec!["lower.txt", "upper.txt"]),
        ("ALPHA", None, vec!["upper.txt"]),
        ("alpha", Some(true), vec!["lower.txt"]),
        ("ALPHA", Some(false), vec!["lower.txt", "upper.txt"]),
        ("^ALPHA$", Some(false), vec!["lower.txt", "upper.txt"]),
        ("(?i)^ALPHA$", Some(true), vec!["lower.txt", "upper.txt"]),
        ("(?-i)^alpha$", Some(false), vec!["lower.txt"]),
    ] {
        let mut arguments = serde_json::json!({
            "pattern": pattern,
            "path": dir,
            "output_mode": "files",
        });
        if let Some(case_sensitive) = case_sensitive {
            arguments["case_sensitive"] = serde_json::json!(case_sensitive);
        }
        let result = execute_structured(&arguments).await.expect("search failed");
        let mut names = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|path| {
                Path::new(path.as_str().unwrap())
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, expected, "{arguments}");
    }
}

#[tokio::test]
async fn content_is_default_and_returns_numbered_lines_with_context() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("controller.rb");
    std::fs::write(
        &file,
        "before\r\n  def current_company # current_company\r\nafter\r\n",
    )
    .unwrap();
    let output = execute_structured(&serde_json::json!({
        "pattern": "current_company",
        "path": file,
        "context_lines": 1,
    }))
    .await
    .unwrap();
    assert_eq!(output["output_mode"], "content");
    assert_eq!(
        output["match_count"], 1,
        "one result per line, not occurrence"
    );
    assert_eq!(output["matches"][0]["path"], serde_json::json!(file));
    assert_eq!(output["matches"][0]["line_number"], 2);
    assert_eq!(
        output["matches"][0]["text"],
        "  def current_company # current_company"
    );
    assert_eq!(output["matches"][0]["truncated"], false);
    assert_eq!(
        output["matches"][0]["context_before"],
        serde_json::json!([{"line_number": 1, "text": "before", "truncated": false}])
    );
    assert_eq!(
        output["matches"][0]["context_after"],
        serde_json::json!([{"line_number": 3, "text": "after", "truncated": false}])
    );
    assert_eq!(output["has_more"], false);
    assert!(output["next_offset"].is_null());
}

#[tokio::test]
async fn content_preserves_multiline_matches_and_empty_line_anchors() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("lines.txt");
    let contents = "prefix\nalpha\nbeta\n\nlast";
    for (body, pattern, expected_lines) in [
        (contents, r"alpha\nbeta", vec![2, 3]),
        (contents, "(?s)alpha.*last", vec![2, 3, 4, 5]),
        (contents, "^$", vec![4]),
        (contents, r"\z", vec![5]),
        (contents, "last", vec![5]),
        (contents, "absent", vec![]),
        ("alpha\n", "^$", vec![2]),
        ("alpha\n", r"\z", vec![2]),
        ("alpha\r\n", r"\z", vec![2]),
        ("alpha\n\n", "^$", vec![2, 3]),
        ("alpha\nbeta\n", r"alpha\nbeta", vec![1, 2]),
        ("alpha\n", "^", vec![1, 2]),
        ("alpha", "^$", vec![]),
        ("\n", "^$", vec![1, 2]),
        ("alpha\n", "alpha", vec![1]),
    ] {
        std::fs::write(&file, body).unwrap();
        let output = execute_structured(&serde_json::json!({
            "pattern": pattern,
            "path": file,
        }))
        .await
        .unwrap();
        let lines: Vec<_> = output["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["line_number"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, expected_lines, "{body:?}: {pattern}");
        if pattern == "^$" || (pattern == r"\z" && body.ends_with('\n')) {
            for entry in output["matches"].as_array().unwrap() {
                assert_eq!(entry["text"], "", "{body:?}: {pattern}");
            }
        }
        assert_eq!(output["has_more"], false);
    }
}

#[tokio::test]
async fn both_modes_paginate_without_losing_matches_or_claiming_more_at_exact_limit() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("a.txt"),
        "needle one\nneedle two\nneedle three\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("b.txt"), "needle four\n").unwrap();
    for (mode, expected_count) in [("content", 4), ("files", 2)] {
        let mut offset = 0;
        let mut matches = Vec::new();
        loop {
            let output = execute_structured(&serde_json::json!({
                "pattern": "needle",
                "path": temp.path(),
                "output_mode": mode,
                "limit": 1,
                "offset": offset,
            }))
            .await
            .unwrap();
            assert_eq!(output["match_count"], 1);
            matches.extend(output["matches"].as_array().unwrap().iter().cloned());
            if !output["has_more"].as_bool().unwrap() {
                assert!(output["next_offset"].is_null());
                break;
            }
            let next = output["next_offset"].as_u64().unwrap();
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(matches.len(), expected_count);
        let unique: std::collections::HashSet<_> =
            matches.iter().map(ToString::to_string).collect();
        assert_eq!(unique.len(), expected_count);
    }
}

#[tokio::test]
async fn snippets_and_pages_are_bounded_and_non_utf8_text_still_matches() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("many.txt");
    let line = format!("needle {}\n", "é".repeat(600));
    std::fs::write(&file, line.repeat(400)).unwrap();
    let output = execute_structured(&serde_json::json!({
        "pattern": "needle",
        "path": file,
        "limit": 2000,
        "context_lines": 2,
    }))
    .await
    .unwrap();
    assert!(output["has_more"].as_bool().unwrap());
    assert!(output["match_count"].as_u64().unwrap() < 400);
    assert_eq!(output["next_offset"], output["match_count"]);
    for entry in output["matches"].as_array().unwrap() {
        assert!(entry["text"].as_str().unwrap().len() <= 500);
        assert_eq!(entry["truncated"], true);
        assert!(!entry["text"].as_str().unwrap().contains('�'));
    }
    assert!(output.to_string().len() < MAX_OUTPUT_BYTES + 1024);

    std::fs::write(&file, [0xff, b'a', b'l', b'p', b'h', b'a', 0xfe]).unwrap();
    let output = execute_structured(&serde_json::json!({"pattern": "alpha", "path": file}))
        .await
        .unwrap();
    assert_eq!(output["matches"][0]["text"], "�alpha�");

    std::fs::write(
        &file,
        format!("{}needle{}", "é".repeat(900), "tail".repeat(500)),
    )
    .unwrap();
    let output = execute_structured(&serde_json::json!({"pattern": "needle", "path": file}))
        .await
        .unwrap();
    let text = output["matches"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("needle"),
        "long-line clipping must retain the match"
    );
    assert!(text.starts_with('…'));
    assert!(!text.contains('�'));
    assert!(text.len() <= 500);
}

#[tokio::test]
async fn search_respects_ignore_rules_and_skips_binary_and_oversized_files() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(temp.path().join("ignored.txt"), "needle").unwrap();
    std::fs::write(temp.path().join("binary.txt"), b"needle\0").unwrap();
    std::fs::write(temp.path().join("keep.txt"), "needle").unwrap();
    let large = File::create(temp.path().join("large.txt")).unwrap();
    large.set_len(MAX_FILE_SIZE + 1).unwrap();
    symlink(temp.path().join("keep.txt"), temp.path().join("alias.txt")).unwrap();
    for mode in ["content", "files"] {
        let output = execute_structured(&serde_json::json!({
            "pattern": "needle",
            "path": temp.path(),
            "output_mode": mode,
        }))
        .await
        .unwrap();
        assert_eq!(output["match_count"], 1, "{output}");
        assert!(output["matches"][0].to_string().contains("keep.txt"));
    }
}

#[tokio::test]
async fn validates_search_options_and_preserves_literal_spaces_in_regexes() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("one.txt");
    std::fs::write(&file, "alpha\n alpha \n").unwrap();
    for invalid in [
        serde_json::json!({"pattern": " "}),
        serde_json::json!({"pattern": "alpha", "limit": 0}),
        serde_json::json!({"pattern": "["}),
        serde_json::json!({"pattern": "alpha", "include": "["}),
        serde_json::json!({"pattern": "alpha", "context_lines": 21}),
        serde_json::json!({"pattern": "alpha", "output_mode": "files", "context_lines": 1}),
        serde_json::json!({"pattern": "alpha", "output_mode": "unknown"}),
        serde_json::json!({"pattern": "alpha", "offset": -1}),
    ] {
        let mut arguments = invalid;
        arguments["path"] = serde_json::json!(file);
        assert!(execute_structured(&arguments).await.is_err(), "{arguments}");
    }
    let output = execute_structured(&serde_json::json!({
        "pattern": " alpha ",
        "path": file,
        "limit": 9000,
    }))
    .await
    .unwrap();
    assert_eq!(output["limit"], MAX_LIMIT);
    assert_eq!(output["match_count"], 1);
    assert_eq!(output["matches"][0]["line_number"], 2);
}
