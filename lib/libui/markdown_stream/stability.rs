//! Stable Markdown prefixes for irreversible scrollback commits.
//!
//! Resolve links against the complete source before withholding mutable blocks.
//! Definitions need not be inside the emitted prefix: they can be in a later
//! paragraph, list, or quote that is itself still waiting for more input.
//!
//! Metadata can begin at later block boundaries too. Probe possible closing
//! delimiters with the same parser rather than maintaining a separate grammar.
//! Probe text is only used for detection; none of its events are rendered.

use std::collections::HashSet;
use std::ops::Range;

use pulldown_cmark::{BrokenLink, CodeBlockKind, Event, Options, Parser, Tag};

pub(super) fn stable_events(source: &str) -> impl Iterator<Item = Event<'_>> {
    let mut first_unresolved = source.len();
    let events: Vec<_> = if contains_footnotes(source) {
        Vec::new()
    } else {
        // Let pulldown-cmark identify full, collapsed, and shortcut references,
        // including images. Brackets in code or escaped text are not references.
        let mut unresolved = |link: BrokenLink<'_>| {
            first_unresolved = first_unresolved.min(link.span.start);
            None
        };
        Parser::new_with_broken_link_callback(
            source,
            crate::markdown_render::parser_options(),
            Some(&mut unresolved),
        )
        .into_offset_iter()
        .collect()
    };
    let first_unstable =
        first_unresolved.min(pending_metadata(source, &events).unwrap_or(source.len()));
    let first_unstable = events
        .iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
            if super::is_mermaid(info) && !super::closed_fence(&source[range.clone()]))
            .then_some(range.start)
        })
        .fold(first_unstable, usize::min);
    let boundary = stable_block_boundary(source, &events, first_unstable);
    events
        .into_iter()
        .take_while(move |(_, range)| range.start < boundary)
        .map(|(event, _)| event)
}

/// Find the earliest existing block that an eventual metadata closer could
/// reinterpret. This handles whitespace, BOMs, and enclosing containers using
/// pulldown-cmark's actual grammar, and releases rules once a blank first body
/// line makes metadata impossible.
fn pending_metadata(source: &str, events: &[(Event<'_>, Range<usize>)]) -> Option<usize> {
    let yaml = events.iter().any(|(event, range)| {
        matches!(event, Event::Rule) && source[range.clone()].trim() == "---"
    });
    let pluses = events
        .iter()
        .any(|(event, _)| matches!(event, Event::Text(text) if text.trim() == "+++"));
    if !yaml && !pluses {
        return None;
    }
    let known: HashSet<_> = events
        .iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Start(Tag::MetadataBlock(_))).then_some(range.start)
        })
        .collect();
    let mut earliest = None;
    for (marker, possible) in [("---", yaml), ("+++", pluses)] {
        if !possible {
            continue;
        }
        // A body line also covers an opener at the very end of the buffer.
        // No leading blank line: that would rule out an otherwise-valid opener.
        let probe = format!("{source}metadata probe\n{marker}\n");
        for (event, range) in
            Parser::new_ext(&probe, crate::markdown_render::parser_options()).into_offset_iter()
        {
            if matches!(event, Event::Start(Tag::MetadataBlock(_)))
                && range.start < source.len()
                && !known.contains(&range.start)
            {
                earliest =
                    Some(earliest.map_or(range.start, |start: usize| start.min(range.start)));
            }
        }
    }
    earliest
}

fn contains_footnotes(source: &str) -> bool {
    if !source.contains("[^") {
        return false;
    }
    // GFM emits undefined footnotes as ordinary text, so it cannot by itself
    // distinguish a pending reference from a literal marker. The old footnote
    // grammar emits dangling references too, while still respecting escapes and
    // literal code/math/HTML/metadata. Use it only for conservative detection;
    // rendering always uses the normal GFM grammar.
    Parser::new_ext(
        source,
        crate::markdown_render::parser_options() | Options::ENABLE_OLD_FOOTNOTES,
    )
    .any(|event| {
        matches!(
            event,
            Event::FootnoteReference(_) | Event::Start(Tag::FootnoteDefinition(_))
        )
    })
}

/// Preserve entire enclosing blocks so styles, prefixes, and spacing stay
/// balanced. Other block kinds (notably code) retain newline-gated streaming.
fn stable_block_boundary(
    source: &str,
    events: &[(Event<'_>, Range<usize>)],
    first_unstable: usize,
) -> usize {
    let table_boundary = crate::table_detect::table_holdback_boundary(source);
    let mut boundary = source.len();
    let mut depth = 0usize;
    let mut trailing = None;
    for (event, range) in events {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    let mutable = match tag {
                        Tag::Paragraph
                        | Tag::List(_)
                        | Tag::BlockQuote(_)
                        | Tag::DefinitionList => true,
                        Tag::Table(_) => !source.ends_with("\n\n") && !source.ends_with("\r\n\r\n"),
                        _ => false,
                    };
                    trailing = Some((range.start, mutable));
                    // The raw table detector is deliberately conservative. Do
                    // not apply its guess inside parser-confirmed literal blocks,
                    // and never cut through the middle of a balanced event stream.
                    let pending_table = !matches!(
                        tag,
                        Tag::CodeBlock(_) | Tag::HtmlBlock | Tag::MetadataBlock(_)
                    ) && table_boundary
                        .is_some_and(|offset| range.contains(&offset));
                    if range.contains(&first_unstable) || pending_table {
                        boundary = boundary.min(range.start);
                    }
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ if depth == 0 => {
                trailing = Some((range.start, false));
                // A YAML opener is currently a standalone Rule, not a Start tag.
                if range.contains(&first_unstable) {
                    boundary = boundary.min(range.start);
                }
            }
            _ => {}
        }
    }
    if let Some((start, true)) = trailing {
        boundary = boundary.min(start);
    }
    boundary
}
