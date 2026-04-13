use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::any::Any;

/// A single renderable unit of conversation history.
///
/// Each cell produces logical `Line`s and reports how many viewport rows those
/// lines occupy at a given terminal width. The default height implementations
/// use `Paragraph::wrap` to account for lines that overflow the viewport width
/// (e.g. long URLs that are kept intact by adaptive wrapping). Concrete types
/// only need to override heights when they apply additional layout logic beyond
/// what `Paragraph::line_count` captures.
pub trait HistoryCell: std::fmt::Debug + Send + Sync + Any {
    /// Returns the logical lines for the main chat viewport.
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;

    /// Returns the number of viewport rows needed to render this cell.
    fn desired_height(&self, width: u16) -> u16 {
        Paragraph::new(Text::from(self.display_lines(width)))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    /// Returns lines for the transcript overlay (`Ctrl+T`).
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.display_lines(width)
    }

    /// Returns the number of viewport rows for the transcript overlay.
    fn desired_transcript_height(&self, width: u16) -> u16 {
        let lines = self.transcript_lines(width);
        // Workaround: ratatui's line_count returns 2 for a single
        // whitespace-only line. Clamp to 1 in that case.
        if let [line] = &lines[..]
            && line
                .spans
                .iter()
                .all(|s| s.content.chars().all(char::is_whitespace))
        {
            return 1;
        }

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    fn is_stream_continuation(&self) -> bool {
        false
    }

    /// Returns a coarse "animation tick" when transcript output is time-dependent.
    fn transcript_animation_tick(&self) -> Option<u64> {
        None
    }
}

impl dyn HistoryCell {
    pub fn as_any(&self) -> &dyn Any {
        self
    }

    pub fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
