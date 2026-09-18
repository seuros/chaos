use super::*;

#[test]
fn parses_basic_scalar() {
    let v = parse_toml_value("42").expect("parse");
    assert_eq!(v.as_integer(), Some(42));
}

#[test]
fn parses_bool() {
    let true_literal = parse_toml_value("true").expect("parse");
    assert_eq!(true_literal.as_bool(), Some(true));

    let false_literal = parse_toml_value("false").expect("parse");
    assert_eq!(false_literal.as_bool(), Some(false));
}

#[test]
fn fails_on_unquoted_string() {
    assert!(parse_toml_value("hello").is_err());
}

#[test]
fn parses_array() {
    let v = parse_toml_value("[1, 2, 3]").expect("parse");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 3);
}

#[test]
fn parses_inline_table() {
    let v = parse_toml_value("{a = 1, b = 2}").expect("parse");
    let tbl = v.as_table().expect("table");
    assert_eq!(tbl.get("a").unwrap().as_integer(), Some(1));
    assert_eq!(tbl.get("b").unwrap().as_integer(), Some(2));
}
