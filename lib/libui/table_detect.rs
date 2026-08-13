//! Pipe-table structure detection and fenced-code tracking over raw markdown.
//!
//! The streaming collector commits completed lines to scrollback as they
//! arrive. That is wrong for a table: the renderer sizes columns from every row
//! it can see, so a row arriving later can widen a column and change how the
//! rows above it should have been drawn. Rows already written to scrollback
//! cannot be redrawn, and the table ends up with a ragged seam where the widths
//! changed mid-stream.
//!
//! [`table_holdback_boundary`] finds where the unfinished table at the end of
//! the committed source begins. Everything from there stays in the stream
//! buffer, gets re-rendered on each delta, and is written once the table is
//! complete — so the final widths are the only ones the terminal ever sees.
//!
//! A GFM pipe table is a header line of pipe-separated segments with at least
//! one non-empty cell, a delimiter line directly beneath it holding only
//! alignment markers (`---`, `:---`, `---:`, `:---:`, three dashes minimum),
//! and body rows after that. Pipes inside a fenced code block are code rather
//! than table syntax, so [`FenceTracker`] classifies each line first.

/// Split a pipe-delimited line into trimmed segments.
///
/// Returns `None` when the line is empty or carries no unescaped separator.
/// Outer pipes are stripped before splitting.
///
/// This is structure detection, not rendering: escaped pipes stay verbatim in
/// the returned segments because callers only ask whether the line could take
/// part in a table, not how its cells should finally read.
pub(crate) fn parse_table_segments(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let has_outer_pipe = trimmed.starts_with('|') || trimmed.ends_with('|');
    let content = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let content = content.strip_suffix('|').unwrap_or(content);
    let raw_segments = split_unescaped_pipe(content);
    if !has_outer_pipe && raw_segments.len() <= 1 {
        return None;
    }

    let segments: Vec<&str> = raw_segments.into_iter().map(str::trim).collect();
    (!segments.is_empty()).then_some(segments)
}

/// Split `content` on unescaped `|` characters.
///
/// A pipe behind a backslash is literal text rather than a column separator.
/// The backslash stays in the segment.
fn split_unescaped_pipe(content: &str) -> Vec<&str> {
    let mut segments = Vec::with_capacity(8);
    let mut start = 0;
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
        } else if bytes[i] == b'|' {
            segments.push(&content[start..i]);
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    segments.push(&content[start..]);
    segments
}

/// Whether `line` could serve as a table header row.
pub(crate) fn is_table_header_line(line: &str) -> bool {
    parse_table_segments(line).is_some_and(|segments| segments.iter().any(|s| !s.is_empty()))
}

/// Whether a segment is one of `---`, `:---`, `---:`, or `:---:`.
fn is_table_delimiter_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return false;
    }
    let without_leading = trimmed.strip_prefix(':').unwrap_or(trimmed);
    let without_ends = without_leading.strip_suffix(':').unwrap_or(without_leading);
    without_ends.len() >= 3 && without_ends.chars().all(|c| c == '-')
}

/// Whether every segment of `line` is an alignment marker.
pub(crate) fn is_table_delimiter_line(line: &str) -> bool {
    parse_table_segments(line)
        .is_some_and(|segments| segments.into_iter().all(is_table_delimiter_segment))
}

/// Peel leading `>` blockquote markers from a line.
///
/// Tables appear inside blockquotes, so the markers come off before the line is
/// checked for table syntax.
pub(crate) fn strip_blockquote_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        let Some(stripped) = rest.strip_prefix('>') else {
            return rest;
        };
        rest = stripped.strip_prefix(' ').unwrap_or(stripped).trim_start();
    }
}

/// Where a source line sits relative to fenced code blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FenceKind {
    /// Not inside any fenced code block.
    Outside,
    /// Inside a ```` ```md ```` or ```` ```markdown ```` fence.
    Markdown,
    /// Inside a fence with some other info string.
    Other,
}

/// Incremental tracker for fenced-code-block open and close transitions.
///
/// Feed lines one at a time through [`advance`](Self::advance) and read the
/// context that applies to the line just fed with [`kind`](Self::kind). The
/// reported kind describes the line *before* that line changes the state, which
/// is what a caller deciding whether the current line can open a table needs.
pub(crate) struct FenceTracker {
    state: Option<(char, usize, FenceKind)>,
}

impl FenceTracker {
    pub(crate) fn new() -> Self {
        Self { state: None }
    }

    /// Process one raw source line and update fence state.
    ///
    /// More than three leading spaces means an indented code block rather than
    /// a fence, so the line is ignored. Blockquote markers come off first.
    pub(crate) fn advance(&mut self, raw_line: &str) {
        let leading_spaces = raw_line
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        if leading_spaces > 3 {
            return;
        }

        let trimmed = &raw_line[leading_spaces..];
        let fence_scan_text = strip_blockquote_prefix(trimmed);
        let Some((marker, len)) = parse_fence_marker(fence_scan_text) else {
            return;
        };

        match self.state {
            // A closing marker must use the same character and run at least as
            // long as the opener, with nothing after it.
            Some((open_char, open_len, _)) => {
                if marker == open_char
                    && len >= open_len
                    && fence_scan_text[len..].trim().is_empty()
                {
                    self.state = None;
                }
            }
            None => {
                let kind = if is_markdown_fence_info(fence_scan_text, len) {
                    FenceKind::Markdown
                } else {
                    FenceKind::Other
                };
                self.state = Some((marker, len, kind));
            }
        }
    }

    /// Fence context for the most recently advanced line.
    pub(crate) fn kind(&self) -> FenceKind {
        self.state.map_or(FenceKind::Outside, |(_, _, kind)| kind)
    }
}

/// Return the fence marker character and run length for a candidate line.
///
/// Leading whitespace and blockquote markers should already be stripped.
fn parse_fence_marker(line: &str) -> Option<(char, usize)> {
    let first = line.as_bytes().first().copied()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let len = line.bytes().take_while(|&byte| byte == first).count();
    (len >= 3).then_some((first as char, len))
}

/// Whether the info string after a fence marker names markdown.
fn is_markdown_fence_info(trimmed_line: &str, marker_len: usize) -> bool {
    let info = trimmed_line[marker_len..]
        .split_whitespace()
        .next()
        .unwrap_or_default();
    info.eq_ignore_ascii_case("md") || info.eq_ignore_ascii_case("markdown")
}

/// One source line, tagged with the fence context that applies to it.
struct ScannedLine<'a> {
    text: &'a str,
    start: usize,
    fence_kind: FenceKind,
}

/// Split `source` into lines carrying their byte offset and fence context.
fn scan_lines(source: &str) -> Vec<ScannedLine<'_>> {
    let mut tracker = FenceTracker::new();
    let mut scanned = Vec::new();
    let mut start = 0usize;

    for raw_line in source.split_inclusive('\n') {
        let text = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        scanned.push(ScannedLine {
            text,
            start,
            fence_kind: tracker.kind(),
        });
        tracker.advance(text);
        start = start.saturating_add(raw_line.len());
    }

    scanned
}

/// Whether a line can take part in a table at all: table-shaped, and not code.
fn is_table_shaped(line: &ScannedLine<'_>) -> bool {
    line.fence_kind != FenceKind::Other
        && parse_table_segments(strip_blockquote_prefix(line.text).trim()).is_some()
}

/// Return the byte offset where the unfinished trailing table starts.
///
/// `None` means the whole of `source` is safe to commit. `Some(offset)` means
/// the caller should commit only `source[..offset]` and leave the rest to be
/// re-rendered as more of the table arrives.
///
/// Only a table running to the end of `source` is held back. A table already
/// closed off by a blank line or a paragraph has its final shape and commits
/// like any other content. A trailing line that reads as a header is held too,
/// on the chance the delimiter row is in the next delta — that costs one line
/// of latency and saves committing a header the renderer would have drawn as
/// prose.
pub(crate) fn table_holdback_boundary(source: &str) -> Option<usize> {
    let lines = scan_lines(source);

    // Walk back over the trailing run of lines that could belong to a table.
    // A blank line, prose, or a code fence ends the run and with it any table
    // whose shape is now settled.
    let mut run_start = lines.len();
    while run_start > 0 && is_table_shaped(&lines[run_start - 1]) {
        run_start -= 1;
    }
    let run = &lines[run_start..];
    if run.is_empty() {
        return None;
    }

    // Within the run, the table proper begins at the first header immediately
    // followed by a delimiter. Prose that merely contains a pipe can sit ahead
    // of a real table without a blank line between them.
    for (index, pair) in run.windows(2).enumerate() {
        let [header, delimiter] = pair else { continue };
        if is_table_header_line(strip_blockquote_prefix(header.text).trim())
            && is_table_delimiter_line(strip_blockquote_prefix(delimiter.text).trim())
        {
            return Some(run[index].start);
        }
    }

    // No delimiter yet. If the very last line reads as a header, the delimiter
    // may still be coming, so hold that line back.
    let last = run.last()?;
    is_table_header_line(strip_blockquote_prefix(last.text).trim()).then_some(last.start)
}

#[cfg(test)]
pub(crate) mod tests {
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
}
