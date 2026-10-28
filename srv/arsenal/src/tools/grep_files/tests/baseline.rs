use super::*;

/// Behavior table for files-only output; content and pagination tests live in the parent.
#[test]
fn search_baseline_metrics() {
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
            label: "limit trims later matches",
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

    let temp = tempfile::tempdir().unwrap();
    let err = run_grep_search("[invalid", None, temp.path(), 10, None).unwrap_err();
    assert!(err.contains("invalid regex"));
}
