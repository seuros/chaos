use super::*;

use std::os::unix::fs::symlink;

/// Behavior table for narrow output mechanics. The deeper semantic tests below
/// exercise multi-line regexes, ordering, file-input mode, and case overrides.
#[test]
fn search_baseline_metrics() {
    // Each row is a (setup, pattern, glob, limit) scenario with its expected
    // hit count; if any row drifts the whole table fails the user-facing
    // contract.
    struct Case<'a> {
        label: &'a str,
        files: &'a [(&'a str, &'a [u8])],
        pattern: &'a str,
        glob: Option<&'a str>,
        limit: usize,
        expect_hits: usize,
        expected_names: &'a [&'a str],
    }
    let cases = [
        Case {
            label: "two matches across files",
            files: &[
                ("match_one.txt", b"alpha beta gamma"),
                ("match_two.txt", b"alpha delta"),
                ("other.txt", b"omega"),
            ],
            pattern: "alpha",
            glob: None,
            limit: 10,
            expect_hits: 2,
            expected_names: &["match_one.txt", "match_two.txt"],
        },
        Case {
            label: "glob narrows to one extension",
            files: &[
                ("match_one.rs", b"alpha beta gamma"),
                ("match_two.txt", b"alpha delta"),
            ],
            pattern: "alpha",
            glob: Some("*.rs"),
            limit: 10,
            expect_hits: 1,
            expected_names: &["match_one.rs"],
        },
        Case {
            label: "limit trims alphabetically late matches",
            files: &[
                ("one.txt", b"alpha one"),
                ("two.txt", b"alpha two"),
                ("three.txt", b"alpha three"),
            ],
            pattern: "alpha",
            glob: None,
            limit: 2,
            expect_hits: 2,
            expected_names: &[],
        },
        Case {
            label: "no hits stays empty",
            files: &[("one.txt", b"omega")],
            pattern: "alpha",
            glob: None,
            limit: 5,
            expect_hits: 0,
            expected_names: &[],
        },
        Case {
            label: "non-UTF-8 still scans against ASCII term",
            files: &[("latin1.txt", &[0xff, b'a', b'l', b'p', b'h', b'a', 0xfe])],
            pattern: "alpha",
            glob: None,
            limit: 10,
            expect_hits: 1,
            expected_names: &["latin1.txt"],
        },
        Case {
            label: "symlink alias is not double-counted",
            files: &[("match.txt", b"alpha beta gamma")],
            pattern: "alpha",
            glob: None,
            limit: 10,
            expect_hits: 1,
            expected_names: &["match.txt"],
        },
    ];
    for case in &cases {
        let temp = tempfile::tempdir().expect("create temp dir");
        for (name, body) in case.files {
            std::fs::write(temp.path().join(name), *body).unwrap();
        }
        if case.label == "symlink alias is not double-counted" {
            let alias = temp.path().join("match-link.txt");
            symlink(temp.path().join("match.txt"), &alias).unwrap();
        }
        let hits = run_grep_search(case.pattern, case.glob, temp.path(), case.limit, None)
            .expect("search failed");
        assert_eq!(
            hits.len(),
            case.expect_hits,
            "{}: {}",
            case.label,
            hits.len()
        );
        for name in case.expected_names {
            assert!(
                hits.iter().any(|p| p.ends_with(*name)),
                "{}: missing {name}, got {hits:?}",
                case.label
            );
        }
    }

    // Invalid regex surfaces a typed error before the regex engine ever
    // touches disk; the empty dir keeps the test cheap.
    let temp = tempfile::tempdir().unwrap();
    let err = run_grep_search("[invalid", None, temp.path(), 10, None).unwrap_err();
    assert!(err.contains("invalid regex"));
}

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
