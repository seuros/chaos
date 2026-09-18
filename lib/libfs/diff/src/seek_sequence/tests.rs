use super::seek_sequence;
use std::string::ToString;

fn to_vec(strings: &[&str]) -> Vec<String> {
    strings.iter().map(ToString::to_string).collect()
}

#[test]
fn test_exact_match_finds_sequence() {
    let lines = to_vec(&["foo", "bar", "baz"]);
    let pattern = to_vec(&["bar", "baz"]);
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
}

#[test]
fn test_rstrip_match_ignores_trailing_whitespace() {
    let lines = to_vec(&["foo   ", "bar\t\t"]);
    // Pattern omits trailing whitespace.
    let pattern = to_vec(&["foo", "bar"]);
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
}

#[test]
fn test_trim_match_ignores_leading_and_trailing_whitespace() {
    let lines = to_vec(&["    foo   ", "   bar\t"]);
    // Pattern omits any additional whitespace.
    let pattern = to_vec(&["foo", "bar"]);
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
}

#[test]
fn test_pattern_longer_than_input_returns_none() {
    let lines = to_vec(&["just one line"]);
    let pattern = to_vec(&["too", "many", "lines"]);
    // Should not panic – must return None when pattern cannot possibly fit.
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), None);
}
