use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn text_formatting_suite() {
    test_truncate_text();
    test_truncate_empty_string();
    test_truncate_max_graphemes_zero();
    test_truncate_max_graphemes_one();
    test_truncate_max_graphemes_two();
    test_truncate_max_graphemes_three_boundary();
    test_truncate_text_shorter_than_limit();
    test_truncate_text_exact_length();
    test_truncate_emoji();
    test_truncate_unicode_combining_characters();
    test_truncate_very_long_text();
    test_format_json_compact_simple_object();
    test_format_json_compact_nested_object();
    test_center_truncate_doesnt_truncate_short_path();
    test_center_truncate_truncates_long_path();
    test_center_truncate_truncates_long_windows_path();
    test_center_truncate_handles_long_segment();
    test_format_json_compact_array();
    test_format_json_compact_already_compact();
    test_format_json_compact_with_whitespace();
    test_format_json_compact_invalid_json();
    test_format_json_compact_empty_object();
    test_format_json_compact_empty_array();
    test_format_json_compact_primitive_values();
    test_proper_join();
}

fn test_truncate_text() {
    let text = "Hello, world!";
    let truncated = truncate_text(text, 8);
    assert_eq!(truncated, "Hello...");
}

fn test_truncate_empty_string() {
    let text = "";
    let truncated = truncate_text(text, 5);
    assert_eq!(truncated, "");
}

fn test_truncate_max_graphemes_zero() {
    let text = "Hello";
    let truncated = truncate_text(text, 0);
    assert_eq!(truncated, "");
}

fn test_truncate_max_graphemes_one() {
    let text = "Hello";
    let truncated = truncate_text(text, 1);
    assert_eq!(truncated, "H");
}

fn test_truncate_max_graphemes_two() {
    let text = "Hello";
    let truncated = truncate_text(text, 2);
    assert_eq!(truncated, "He");
}

fn test_truncate_max_graphemes_three_boundary() {
    let text = "Hello";
    let truncated = truncate_text(text, 3);
    assert_eq!(truncated, "...");
}

fn test_truncate_text_shorter_than_limit() {
    let text = "Hi";
    let truncated = truncate_text(text, 10);
    assert_eq!(truncated, "Hi");
}

fn test_truncate_text_exact_length() {
    let text = "Hello";
    let truncated = truncate_text(text, 5);
    assert_eq!(truncated, "Hello");
}

fn test_truncate_emoji() {
    let text = "👋🌍🚀✨💫";
    let truncated = truncate_text(text, 3);
    assert_eq!(truncated, "...");

    let truncated_longer = truncate_text(text, 4);
    assert_eq!(truncated_longer, "👋...");
}

fn test_truncate_unicode_combining_characters() {
    let text = "é́ñ̃"; // Characters with combining marks
    let truncated = truncate_text(text, 2);
    assert_eq!(truncated, "é́ñ̃");
}

fn test_truncate_very_long_text() {
    let text = "a".repeat(1000);
    let truncated = truncate_text(&text, 10);
    assert_eq!(truncated, "aaaaaaa...");
    assert_eq!(truncated.len(), 10); // 7 'a's + 3 dots
}

fn test_format_json_compact_simple_object() {
    let json = r#"{ "name": "John", "age": 30 }"#;
    let result = format_json_compact(json).unwrap();
    assert_eq!(result, r#"{"name": "John", "age": 30}"#);
}

fn test_format_json_compact_nested_object() {
    let json = r#"{ "user": { "name": "John", "details": { "age": 30, "city": "NYC" } } }"#;
    let result = format_json_compact(json).unwrap();
    assert_eq!(
        result,
        r#"{"user": {"name": "John", "details": {"age": 30, "city": "NYC"}}}"#
    );
}

fn test_center_truncate_doesnt_truncate_short_path() {
    let sep = std::path::MAIN_SEPARATOR;
    let path = format!("{sep}Users{sep}chaos{sep}Public");
    let truncated = center_truncate_path(&path, 40);

    assert_eq!(truncated, path);
}

fn test_center_truncate_truncates_long_path() {
    let sep = std::path::MAIN_SEPARATOR;
    let path = format!("~{sep}hello{sep}the{sep}fox{sep}is{sep}very{sep}fast");
    let truncated = center_truncate_path(&path, 24);

    assert_eq!(
        truncated,
        format!("~{sep}hello{sep}the{sep}…{sep}very{sep}fast")
    );
}

fn test_center_truncate_truncates_long_windows_path() {
    let sep = std::path::MAIN_SEPARATOR;
    let path = format!(
        "C:{sep}Users{sep}chaos{sep}Projects{sep}super{sep}long{sep}windows{sep}path{sep}file.txt"
    );
    let truncated = center_truncate_path(&path, 36);

    let expected = format!("C:{sep}Users{sep}chaos{sep}…{sep}path{sep}file.txt");

    assert_eq!(truncated, expected);
}

fn test_center_truncate_handles_long_segment() {
    let sep = std::path::MAIN_SEPARATOR;
    let path = format!("~{sep}supercalifragilisticexpialidocious");
    let truncated = center_truncate_path(&path, 18);

    assert_eq!(truncated, format!("~{sep}…cexpialidocious"));
}

fn test_format_json_compact_array() {
    let json = r#"[ 1, 2, { "key": "value" }, "string" ]"#;
    let result = format_json_compact(json).unwrap();
    assert_eq!(result, r#"[1, 2, {"key": "value"}, "string"]"#);
}

fn test_format_json_compact_already_compact() {
    let json = r#"{"compact":true}"#;
    let result = format_json_compact(json).unwrap();
    assert_eq!(result, r#"{"compact": true}"#);
}

fn test_format_json_compact_with_whitespace() {
    let json = r#"
        {
            "name": "John",
            "hobbies": [
                "reading",
                "coding"
            ]
        }
        "#;
    let result = format_json_compact(json).unwrap();
    assert_eq!(
        result,
        r#"{"name": "John", "hobbies": ["reading", "coding"]}"#
    );
}

fn test_format_json_compact_invalid_json() {
    let invalid_json = r#"{"invalid": json syntax}"#;
    let result = format_json_compact(invalid_json);
    assert!(result.is_none());
}

fn test_format_json_compact_empty_object() {
    let json = r#"{}"#;
    let result = format_json_compact(json).unwrap();
    assert_eq!(result, "{}");
}

fn test_format_json_compact_empty_array() {
    let json = r#"[]"#;
    let result = format_json_compact(json).unwrap();
    assert_eq!(result, "[]");
}

fn test_format_json_compact_primitive_values() {
    assert_eq!(format_json_compact("42").unwrap(), "42");
    assert_eq!(format_json_compact("true").unwrap(), "true");
    assert_eq!(format_json_compact("false").unwrap(), "false");
    assert_eq!(format_json_compact("null").unwrap(), "null");
    assert_eq!(format_json_compact(r#""string""#).unwrap(), r#""string""#);
}

fn test_proper_join() {
    let empty: Vec<String> = vec![];
    assert_eq!(proper_join(&empty), "");
    assert_eq!(proper_join(&["apple"]), "apple");
    assert_eq!(proper_join(&["apple", "banana"]), "apple and banana");
    assert_eq!(
        proper_join(&["apple", "banana", "cherry"]),
        "apple, banana and cherry"
    );
    assert_eq!(
        proper_join(&["apple", "banana", "cherry", "date"]),
        "apple, banana, cherry and date"
    );
}
