use ratatui::text::Line;
use std::path::Path;
use std::path::PathBuf;

use crate::markdown_render::MarkdownPart;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};

#[cfg(test)]
use crate::markdown;

mod stability;

/// Newline-gated accumulator that commits only stable Markdown blocks/lines.
pub struct MarkdownStreamCollector {
    buffer: String,
    committed_part_count: usize,
    width: Option<usize>,
    cwd: PathBuf,
}

impl MarkdownStreamCollector {
    /// Create a collector that renders markdown using `cwd` for local file-link display.
    ///
    /// The collector snapshots `cwd` into owned state because stream commits can happen long after
    /// construction. The same `cwd` should be reused for the entire stream lifecycle; mixing
    /// different working directories within one stream would make the same link render with
    /// different path prefixes across incremental commits.
    pub fn new(width: Option<usize>, cwd: &Path) -> Self {
        Self {
            buffer: String::new(),
            committed_part_count: 0,
            width,
            cwd: cwd.to_path_buf(),
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.committed_part_count = 0;
    }

    pub fn push_delta(&mut self, delta: &str) {
        tracing::trace!("push_delta: {delta:?}");
        self.buffer.push_str(delta);
    }

    /// Render the full buffer and return only the newly completed logical lines
    /// since the last commit. When the buffer does not end with a newline, the
    /// final rendered line is considered incomplete and is not emitted.
    ///
    /// A table still taking on rows is incomplete in the same sense even though
    /// its rows end in newlines: the renderer sizes columns from every row it
    /// can see, so a later row can change how earlier ones should be drawn.
    /// Those rows are withheld until the table closes or the stream finalizes.
    /// Potential metadata openers wait for a closer or a blank first body line
    /// that makes metadata impossible, including at later block boundaries.
    /// Trailing prose/list/quote blocks are also held: a later definition-list
    /// description can turn their last paragraph into a styled term. Footnote
    /// documents wait for finalization because definitions can resolve earlier
    /// references, even across otherwise-complete paragraphs. Literal markers
    /// in code, math, HTML, or metadata do not trigger that holdback.
    ///
    /// Unresolved reference links/images hold their enclosing block and everything
    /// after it. Once definitions arrive, stable blocks resume with resolved
    /// events from the full source, even if a definition lies beyond the cutoff.
    pub fn commit_complete_lines(&mut self) -> Vec<Line<'static>> {
        self.commit_complete_parts()
            .into_iter()
            .flat_map(MarkdownPart::into_lines)
            .collect()
    }

    pub(crate) fn commit_complete_parts(&mut self) -> Vec<MarkdownPart> {
        let Some(last_newline_idx) = self.buffer.rfind('\n') else {
            return Vec::new();
        };
        let source = &self.buffer[..=last_newline_idx];
        let mut rendered = crate::markdown_render::render_stream_parts(
            stability::stable_events(source),
            self.width,
            &self.cwd,
        );
        let mut complete_part_count = rendered.len();
        if complete_part_count > 0
            && matches!(&rendered[complete_part_count - 1], MarkdownPart::Line(line)
                if crate::render::line_utils::is_blank_line_spaces_only(line))
        {
            complete_part_count -= 1;
        }

        if self.committed_part_count >= complete_part_count {
            return Vec::new();
        }

        rendered.truncate(complete_part_count);
        rendered.drain(..self.committed_part_count);
        self.committed_part_count = complete_part_count;
        rendered
    }

    /// Finalize the stream: emit all remaining lines beyond the last commit.
    /// If the buffer does not end with a newline, a temporary one is appended
    /// for rendering.
    pub fn finalize_and_drain(&mut self) -> Vec<Line<'static>> {
        self.finalize_parts()
            .into_iter()
            .flat_map(MarkdownPart::into_lines)
            .collect()
    }

    pub(crate) fn finalize_parts(&mut self) -> Vec<MarkdownPart> {
        let mut source = std::mem::take(&mut self.buffer);
        let raw_len = source.len();
        if !source.ends_with('\n') {
            source.push('\n');
        }
        tracing::debug!(
            raw_len,
            source_len = source.len(),
            "markdown finalize (raw length: {}, rendered length: {})",
            raw_len,
            source.len()
        );
        tracing::trace!("markdown finalize (raw source):\n---\n{source}\n---");

        let events = Parser::new_ext(&source, crate::markdown_render::parser_options())
            .into_offset_iter()
            .map(|(event, range)| match event {
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info)))
                    if is_mermaid(info) && !closed_fence(&source[range]) =>
                {
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced("text".into())))
                }
                _ => event,
            });
        let mut rendered =
            crate::markdown_render::render_stream_parts(events, self.width, &self.cwd);

        let out = if self.committed_part_count >= rendered.len() {
            Vec::new()
        } else {
            rendered.drain(..self.committed_part_count);
            rendered
        };

        // Reset collector state for next stream.
        self.clear();
        out
    }
}

pub(super) fn is_mermaid(info: &str) -> bool {
    info.split([',', ' ', '\t'])
        .next()
        .is_some_and(|lang| lang.eq_ignore_ascii_case("mermaid"))
}

/// The parser supplies the entire fenced block range, including a closer if
/// present. An implicit EOF end is not a complete diagram.
pub(super) fn closed_fence(block: &str) -> bool {
    let mut lines = block.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    let first = first.trim_start();
    let Some(marker @ ('`' | '~')) = first.chars().next() else {
        return false;
    };
    let count = first.chars().take_while(|c| *c == marker).count();
    lines.last().is_some_and(|last| {
        let unindented = last.trim_start_matches(' ');
        let indentation = last.len() - unindented.len();
        let closer = unindented.trim_end_matches([' ', '\t', '\r']);
        indentation <= 3 && closer.len() >= count && closer.chars().all(|c| c == marker)
    })
}

#[cfg(test)]
fn test_cwd() -> PathBuf {
    // These tests only need a stable absolute cwd; using temp_dir() avoids baking Unix- or
    // Windows-specific root semantics into the fixtures.
    std::env::temp_dir()
}

#[cfg(test)]
pub fn simulate_stream_markdown_for_tests(deltas: &[&str], finalize: bool) -> Vec<Line<'static>> {
    let mut collector = MarkdownStreamCollector::new(None, &test_cwd());
    let mut out = Vec::new();
    for d in deltas {
        collector.push_delta(d);
        if d.contains('\n') {
            out.extend(collector.commit_complete_lines());
        }
    }
    if finalize {
        out.extend(collector.finalize_and_drain());
    }
    out
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod reference_tests;

#[cfg(test)]
mod metadata_tests;
