use super::*;
use crate::activity::command::classify;
use pretty_assertions::assert_eq;

fn check_of(script: &str) -> Option<CheckKind> {
    classify(&["bash".to_string(), "-lc".to_string(), script.to_string()]).check
}

#[test]
fn checks_are_recognised_through_runners_and_wrappers() {
    let cases = [
        (
            "cargo test -p chaos-kern 2>&1 | tail",
            Some(CheckKind::Test),
        ),
        ("cargo nextest run", Some(CheckKind::Test)),
        (
            "cargo fmt --check && cargo clippy --tests",
            Some(CheckKind::Lint),
        ),
        ("cargo fmt", None),
        ("cargo check && cargo test", Some(CheckKind::Test)),
        ("cargo build --release", Some(CheckKind::Build)),
        ("bundle exec rake test", Some(CheckKind::Test)),
        ("bin/rails test test/models", Some(CheckKind::Test)),
        ("bundle exec rubocop -a", Some(CheckKind::Lint)),
        ("bun test", Some(CheckKind::Test)),
        ("bun run build", Some(CheckKind::Build)),
        ("mise run test:unit", Some(CheckKind::Test)),
        ("mise exec -- pytest -q", Some(CheckKind::Test)),
        ("python3 -m pytest", Some(CheckKind::Test)),
        ("cargo run -- serve", None),
    ];
    for (script, kind) in cases {
        assert_eq!(check_of(script), kind, "{script}");
    }
}

#[test]
fn counts_come_from_each_runner_summary() {
    let cases = [
        (
            "test result: ok. 12 passed; 0 failed; 1 ignored; 0 measured\n\
             test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured",
            (15, 2, 1),
        ),
        (
            "     Summary [   0.412s] 14 tests run: 13 passed, 1 failed, 2 skipped",
            (13, 1, 2),
        ),
        (
            "10 runs, 25 assertions, 1 failures, 1 errors, 2 skips",
            (6, 2, 2),
        ),
        (
            "===== 1 failed, 5 passed, 2 skipped in 0.12s =====",
            (5, 1, 2),
        ),
        (" 5 pass\n 1 fail\n 0 skip\n", (5, 1, 0)),
    ];
    for (output, (passed, failed, skipped)) in cases {
        assert_eq!(
            counts(output),
            Some(CheckCounts {
                passed,
                failed,
                skipped
            }),
            "{output}"
        );
    }
    assert_eq!(counts("Finished `release` profile"), None);
}

#[test]
fn every_summary_pattern_compiles() {
    for pattern in [&CARGO, &NEXTEST, &MINITEST, &PYTEST, &BUN] {
        assert!(pattern.is_some());
    }
}
