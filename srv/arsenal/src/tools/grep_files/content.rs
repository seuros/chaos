//! Line-numbered, bounded snippets from whole-file byte-regex matches.

use std::path::Path;

use chaos_wchar::take_bytes_at_char_boundary;
use regex::bytes::Regex;
use serde::Serialize;

const MAX_LINE_BYTES: usize = 500;

#[derive(Debug, Serialize)]
pub(super) struct ContentMatch {
    path: String,
    #[serde(flatten)]
    line: ContentLine,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_before: Vec<ContentLine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_after: Vec<ContentLine>,
}

#[derive(Debug, Serialize)]
struct ContentLine {
    line_number: usize,
    text: String,
    truncated: bool,
}

pub(super) struct MatchedLine {
    number: usize,
    start: usize,
    end: usize,
    focus: usize,
}

/// Emit each covered line once, including lines covered by a multi-line match.
pub(super) fn matching_lines<'a>(
    contents: &'a [u8],
    regex: &'a Regex,
) -> impl Iterator<Item = MatchedLine> + 'a {
    let mut matches = regex.find_iter(contents).peekable();
    let mut offset = 0;
    // A trailing newline puts EOF anchors on a new, empty line.
    let eof_line = contents.ends_with(b"\n").then_some(&contents[contents.len()..]);
    contents
        .split_inclusive(|byte| *byte == b'\n')
        .chain(eof_line)
        .enumerate()
        .filter_map(move |(index, bytes)| {
            let start = offset;
            offset += bytes.len();
            while matches
                .peek()
                .is_some_and(|hit| hit.end() <= start && !(hit.is_empty() && hit.start() == start))
            {
                matches.next();
            }
            let hit = matches.peek()?;
            let covered = hit.start() < offset
                || (hit.is_empty()
                    && hit.start() == offset
                    && offset == contents.len()
                    && !bytes.ends_with(b"\n"));
            covered.then_some(MatchedLine {
                number: index + 1,
                start,
                end: offset,
                focus: hit.start().saturating_sub(start),
            })
        })
}

pub(super) fn snippet(
    path: &Path,
    contents: &[u8],
    line: MatchedLine,
    context_lines: usize,
) -> ContentMatch {
    let mut context_before: Vec<_> = contents[..line.start]
        .split_inclusive(|byte| *byte == b'\n')
        .rev()
        .take(context_lines)
        .enumerate()
        .map(|(index, bytes)| content_line(line.number - index - 1, bytes, 0))
        .collect();
    context_before.reverse();
    let context_after = contents[line.end..]
        .split_inclusive(|byte| *byte == b'\n')
        .take(context_lines)
        .enumerate()
        .map(|(index, bytes)| content_line(line.number + index + 1, bytes, 0))
        .collect();
    ContentMatch {
        path: path.to_string_lossy().into_owned(),
        line: content_line(line.number, &contents[line.start..line.end], line.focus),
        context_before,
        context_after,
    }
}

fn content_line(line_number: usize, bytes: &[u8], focus: usize) -> ContentLine {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let text = String::from_utf8_lossy(bytes);
    // Keep the match visible even in minified files or lines with long prefixes.
    // Convert the focus to decoded-text bytes so non-UTF-8 input stays searchable.
    let focus = String::from_utf8_lossy(&bytes[..focus.min(bytes.len())]).len();
    let start = if focus >= MAX_LINE_BYTES {
        take_bytes_at_char_boundary(&text, focus.saturating_sub(MAX_LINE_BYTES / 4)).len()
    } else {
        0
    };
    let prefix = if start == 0 { "" } else { "…" };
    let excerpt = take_bytes_at_char_boundary(&text[start..], MAX_LINE_BYTES - prefix.len());
    let truncated = start != 0 || excerpt.len() != text.len();
    ContentLine {
        line_number,
        text: format!("{prefix}{excerpt}"),
        truncated,
    }
}
