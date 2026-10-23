use super::seek_sequence;
use std::string::ToString;

fn to_vec(strings: &[&str]) -> Vec<String> {
    strings.iter().map(ToString::to_string).collect()
}

#[test]
fn matches_exact_then_whitespace_fallbacks_and_rejects_overlong_patterns() {
    for (lines, pattern, expected) in [
        (&["foo", "bar", "baz"][..], &["bar", "baz"][..], Some(1)),
        (&["foo   ", "bar\t\t"], &["foo", "bar"], Some(0)),
        (&["    foo   ", "   bar\t"], &["foo", "bar"], Some(0)),
        (&["just one line"], &["too", "many", "lines"], None),
    ] {
        assert_eq!(
            seek_sequence(&to_vec(lines), &to_vec(pattern), 0, false),
            expected,
            "lines: {lines:?}, pattern: {pattern:?}",
        );
    }
}
