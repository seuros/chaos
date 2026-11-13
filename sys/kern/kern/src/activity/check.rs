//! Whether a command is a check (tests, lints, type checks, builds), and the
//! counts it reported. Only the counts leave; the output never does.

use std::sync::LazyLock;

use regex_lite::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Build,
    Typecheck,
    Lint,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CheckCounts {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

/// The check a single command runs, if it is one.
pub(super) fn kind(program: &str, args: &[String]) -> Option<CheckKind> {
    let first = args.first().map(String::as_str);
    match (program, first) {
        ("cargo", Some("test" | "nextest")) => Some(CheckKind::Test),
        ("cargo", Some("clippy")) => Some(CheckKind::Lint),
        ("cargo", Some("fmt")) if args.iter().any(|arg| arg == "--check") => Some(CheckKind::Lint),
        ("cargo", Some("check")) => Some(CheckKind::Typecheck),
        ("cargo", Some("build")) => Some(CheckKind::Build),
        ("bun", Some("test")) | ("pytest", _) => Some(CheckKind::Test),
        ("python" | "python3", Some("-m")) if args.get(1).is_some_and(|m| m == "pytest") => {
            Some(CheckKind::Test)
        }
        ("rake", Some("test")) | ("rails", Some("test")) => Some(CheckKind::Test),
        ("rubocop", _) => Some(CheckKind::Lint),
        ("bun" | "mise", Some("run")) => args.get(1).and_then(|task| task_kind(task)),
        _ => None,
    }
}

/// A task runner's task, judged by its name.
fn task_kind(task: &str) -> Option<CheckKind> {
    match task.split([':', '-']).next().unwrap_or(task) {
        "test" | "tests" => Some(CheckKind::Test),
        "lint" | "clippy" | "fmt" => Some(CheckKind::Lint),
        "check" | "typecheck" => Some(CheckKind::Typecheck),
        "build" => Some(CheckKind::Build),
        _ => None,
    }
}

static CARGO: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored").ok()
});
static NEXTEST: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(concat!(
        r"Summary \[[^\]]*\] +\d+ tests? run: (\d+) passed",
        r"(?:, (\d+) failed)?(?:.*?, (\d+) skipped)?",
    ))
    .ok()
});
static MINITEST: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(\d+) runs, \d+ assertions, (\d+) failures, (\d+) errors, (\d+) skips").ok()
});
static PYTEST: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"=+ (?:(\d+) failed, )?(\d+) passed(?:, (\d+) skipped)?.* in [\d.]+s").ok()
});
static BUN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(\d+) (pass|fail|skip)\s*$").ok());

/// Test counts from the runners above, summed when a command ran several.
pub(super) fn counts(output: &str) -> Option<CheckCounts> {
    let number = |m: Option<regex_lite::Match<'_>>| {
        m.and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0)
    };
    let mut total: Option<CheckCounts> = None;
    let mut add = |passed, failed, skipped| {
        let sum = total.get_or_insert(CheckCounts {
            passed: 0,
            failed: 0,
            skipped: 0,
        });
        sum.passed += passed;
        sum.failed += failed;
        sum.skipped += skipped;
    };
    for c in CARGO.iter().flat_map(|re| re.captures_iter(output)) {
        add(number(c.get(1)), number(c.get(2)), number(c.get(3)));
    }
    for c in NEXTEST.iter().flat_map(|re| re.captures_iter(output)) {
        add(number(c.get(1)), number(c.get(2)), number(c.get(3)));
    }
    for c in MINITEST.iter().flat_map(|re| re.captures_iter(output)) {
        let (runs, failures, errors, skips) = (
            number(c.get(1)),
            number(c.get(2)),
            number(c.get(3)),
            number(c.get(4)),
        );
        let failed = failures + errors;
        add(runs.saturating_sub(failed + skips), failed, skips);
    }
    for c in PYTEST.iter().flat_map(|re| re.captures_iter(output)) {
        add(number(c.get(2)), number(c.get(1)), number(c.get(3)));
    }
    let mut bun = [0u64; 3];
    let mut saw_bun = false;
    for c in BUN.iter().flat_map(|re| re.captures_iter(output)) {
        saw_bun = true;
        let slot = match &c[2] {
            "pass" => 0,
            "fail" => 1,
            _ => 2,
        };
        bun[slot] += number(c.get(1));
    }
    if saw_bun {
        add(bun[0], bun[1], bun[2]);
    }
    total
}

#[cfg(test)]
mod tests;
