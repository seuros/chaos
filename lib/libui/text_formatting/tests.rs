use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn text_formatting_suite() {
    truncate_text_respects_graphemes_and_ellipsis_budget();
    compact_json_stays_wrappable_without_changing_strings();
    center_truncate_keeps_useful_path_segments();
    proper_join_uses_english_separators();
}

fn truncate_text_respects_graphemes_and_ellipsis_budget() {
    for (text, limit, expected) in [
        ("", 5, ""),
        ("Hello", 0, ""),
        ("Hello", 1, "H"),
        ("Hello", 2, "He"),
        ("Hello", 3, "..."),
        ("Hello", 5, "Hello"),
        ("Hi", 10, "Hi"),
        ("Hello, world!", 8, "Hello..."),
        ("👋🌍🚀✨💫", 3, "..."),
        ("👋🌍🚀✨💫", 4, "👋..."),
        ("é́ñ̃", 2, "é́ñ̃"),
    ] {
        assert_eq!(truncate_text(text, limit), expected, "{text:?} at {limit}");
    }
    assert_eq!(truncate_text(&"a".repeat(1000), 10), "aaaaaaa...");
}

fn compact_json_stays_wrappable_without_changing_strings() {
    for (input, expected) in [
        (
            r#"{ "name": "John", "age": 30 }"#,
            r#"{"name": "John", "age": 30}"#,
        ),
        (
            r#"{ "user": { "name": "John", "details": { "age": 30, "city": "NYC" } } }"#,
            r#"{"user": {"name": "John", "details": {"age": 30, "city": "NYC"}}}"#,
        ),
        (
            r#"[ 1, 2, { "key": "value" }, "string" ]"#,
            r#"[1, 2, {"key": "value"}, "string"]"#,
        ),
        (
            "{\n  \"name\": \"John\",\n  \"hobbies\": [\"reading\", \"coding\"]\n}",
            r#"{"name": "John", "hobbies": ["reading", "coding"]}"#,
        ),
        (r#"{"message":"a,b:c"}"#, r#"{"message": "a,b:c"}"#),
    ] {
        assert_eq!(
            format_json_compact(input).as_deref(),
            Some(expected),
            "{input}"
        );
    }
    assert!(format_json_compact(r#"{"invalid": json syntax}"#).is_none());
}

fn center_truncate_keeps_useful_path_segments() {
    let sep = std::path::MAIN_SEPARATOR;
    for (path, width, expected) in [
        (
            format!("{sep}Users{sep}chaos{sep}Public"),
            40,
            format!("{sep}Users{sep}chaos{sep}Public"),
        ),
        (
            format!("~{sep}hello{sep}the{sep}fox{sep}is{sep}very{sep}fast"),
            24,
            format!("~{sep}hello{sep}the{sep}…{sep}very{sep}fast"),
        ),
        (
            format!(
                "C:{sep}Users{sep}chaos{sep}Projects{sep}super{sep}long{sep}windows{sep}path{sep}file.txt"
            ),
            36,
            format!("C:{sep}Users{sep}chaos{sep}…{sep}path{sep}file.txt"),
        ),
        (
            format!("~{sep}supercalifragilisticexpialidocious"),
            18,
            format!("~{sep}…cexpialidocious"),
        ),
    ] {
        assert_eq!(center_truncate_path(&path, width), expected, "{path}");
    }
}

fn proper_join_uses_english_separators() {
    for (items, expected) in [
        (&[][..], ""),
        (&["apple"][..], "apple"),
        (&["apple", "banana"][..], "apple and banana"),
        (
            &["apple", "banana", "cherry"][..],
            "apple, banana and cherry",
        ),
        (
            &["apple", "banana", "cherry", "date"][..],
            "apple, banana, cherry and date",
        ),
    ] {
        assert_eq!(proper_join(items), expected, "{items:?}");
    }
}
