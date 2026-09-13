use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn table_detect_suite() {
    table_structure_is_recognised_by_shape();
    fence_tracker_follows_open_and_close_markers();
    holdback_covers_the_unfinished_trailing_table();
}

/// Header, delimiter, and segment splitting across the shapes that decide
/// whether a line can join a table.
fn table_structure_is_recognised_by_shape() {
    assert_eq!(
        parse_table_segments("| A | B | C |"),
        Some(vec!["A", "B", "C"])
    );
    assert_eq!(parse_table_segments("A | B | C"), Some(vec!["A", "B", "C"]));
    assert_eq!(parse_table_segments("| only |"), Some(vec!["only"]));
    assert_eq!(parse_table_segments("just text"), None);
    assert_eq!(parse_table_segments("   "), None);
    // An escaped pipe is cell text, not a column boundary.
    assert_eq!(
        parse_table_segments(r"| A \| B | C |"),
        Some(vec![r"A \| B", "C"])
    );

    assert!(is_table_header_line("| A | B |"));
    assert!(is_table_header_line("Name | Value"));
    assert!(!is_table_header_line("| | |"));

    assert!(is_table_delimiter_line("| --- | --- |"));
    assert!(is_table_delimiter_line("|:---:|---:|"));
    assert!(is_table_delimiter_line("--- | --- | ---"));
    // Two dashes is short of the three a delimiter needs.
    assert!(!is_table_delimiter_line("| -- | -- |"));
    assert!(!is_table_delimiter_line("| A | B |"));

    assert_eq!(strip_blockquote_prefix("> > nested"), "nested");
    assert_eq!(strip_blockquote_prefix("no prefix"), "no prefix");
}

/// Marker length, marker character, indentation, and info strings all gate
/// whether a fence opens or closes.
fn fence_tracker_follows_open_and_close_markers() {
    let mut tracker = FenceTracker::new();
    assert_eq!(tracker.kind(), FenceKind::Outside);

    tracker.advance("````sh");
    assert_eq!(tracker.kind(), FenceKind::Other);
    // Too short to close, wrong character to close, trailing content to close.
    tracker.advance("```");
    tracker.advance("~~~~");
    tracker.advance("```` extra");
    assert_eq!(tracker.kind(), FenceKind::Other);
    tracker.advance("````");
    assert_eq!(tracker.kind(), FenceKind::Outside);

    tracker.advance("> ```Markdown");
    assert_eq!(tracker.kind(), FenceKind::Markdown);
    tracker.advance("> ```");
    assert_eq!(tracker.kind(), FenceKind::Outside);

    // Four leading spaces is an indented code block, not a fence.
    tracker.advance("    ```sh");
    assert_eq!(tracker.kind(), FenceKind::Outside);
}

/// The boundary tracks a table only while it is still growing.
fn holdback_covers_the_unfinished_trailing_table() {
    // Nothing table-shaped at the end.
    assert_eq!(table_holdback_boundary("plain prose\n"), None);
    assert_eq!(table_holdback_boundary(""), None);

    // A trailing header alone is held on the chance a delimiter follows.
    let pending = "intro\n| A | B |\n";
    assert_eq!(table_holdback_boundary(pending), Some(6));

    // Header plus delimiter plus rows: hold from the header.
    let growing = "intro\n| A | B |\n| --- | --- |\n| 1 | 2 |\n";
    assert_eq!(table_holdback_boundary(growing), Some(6));

    // A blank line closes the table, so its widths are final.
    let closed = "| A | B |\n| --- | --- |\n| 1 | 2 |\n\n";
    assert_eq!(table_holdback_boundary(closed), None);

    // A second table after a closed one holds only the second.
    let two = "| A |\n| --- |\n| 1 |\n\n| C | D |\n| --- | --- |\n";
    assert_eq!(table_holdback_boundary(two), Some(21));

    // Prose carrying a pipe directly above a table is not part of it.
    let prose_above = "a | b is prose\n| A | B |\n| --- | --- |\n";
    assert_eq!(table_holdback_boundary(prose_above), Some(15));

    // Pipes inside a code fence are code.
    let fenced = "```sh\n| A | B |\n| --- | --- |\n";
    assert_eq!(table_holdback_boundary(fenced), None);

    // A markdown fence still holds real tables.
    let md_fenced = "```md\n| A | B |\n| --- | --- |\n";
    assert_eq!(table_holdback_boundary(md_fenced), Some(6));

    // Quoted tables count as tables.
    let quoted = "> | A | B |\n> | --- | --- |\n";
    assert_eq!(table_holdback_boundary(quoted), Some(0));
}
