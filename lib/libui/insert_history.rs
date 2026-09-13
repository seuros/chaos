use std::fmt;
use std::io;
use std::io::Write;

use crate::modifier_diff::ModifierDiff;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use crate::wrapping::line_contains_url_like;
use crate::wrapping::line_has_mixed_url_and_non_url_tokens;
use crossterm::Command;
use crossterm::cursor::MoveDown;
use crossterm::cursor::MoveTo;
use crossterm::cursor::MoveToColumn;
use crossterm::cursor::RestorePosition;
use crossterm::cursor::SavePosition;
use crossterm::queue;
use crossterm::style::Color as CColor;
use crossterm::style::Colors;
use crossterm::style::Print;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetColors;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use ratatui::prelude::Backend;
use ratatui::prelude::IntoCrossterm;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;

#[cfg(all(test, feature = "vt100-tests"))]
mod scrollback_tests;

/// Insert `lines` above the viewport using the terminal's backend writer
/// (avoids direct stdout references). The caller must redraw the viewport.
pub fn insert_history_lines<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    lines: Vec<Line>,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    insert_history_lines_inner(terminal, lines, 0)
}

/// Like [`insert_history_lines`] but leaves room for `top_reserved_rows` of
/// chrome. The caller must repaint these rows after insertion, in the same
/// synchronized update, so chrome never enters terminal-native scrollback.
pub fn insert_history_lines_with_reserved<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    lines: Vec<Line>,
    top_reserved_rows: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    insert_history_lines_inner(terminal, lines, top_reserved_rows)
}

fn insert_history_lines_inner<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    lines: Vec<Line>,
    top_reserved_rows: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    if lines.is_empty() {
        return Ok(());
    }
    let screen_size = terminal.size()?;
    if screen_size.width == 0 || screen_size.height == 0 {
        return Ok(());
    }
    let mut area = terminal.viewport_area;
    let reserved = top_reserved_rows.min(area.top());

    // Pre-wrap lines for terminal scrollback. Three paths:
    //
    // - URL-only-ish lines are kept intact (no hard newlines inserted) so that
    //   terminal emulators can match them as clickable links. The
    //   terminal will character-wrap these lines at the viewport
    //   boundary.
    // - Mixed lines (URL + non-URL prose) are adaptively wrapped so
    //   non-URL text still wraps naturally while URL tokens remain
    //   unsplit.
    // - Non-URL lines also flow through adaptive wrapping; behavior is
    //   equivalent to standard wrapping when no URL is present.
    let wrap_width = area.width.max(1) as usize;
    let mut wrapped = Vec::new();
    let mut wrapped_rows = 0usize;

    for line in &lines {
        let line_wrapped =
            if line_contains_url_like(line) && !line_has_mixed_url_and_non_url_tokens(line) {
                vec![line.clone()]
            } else {
                adaptive_wrap_line(line, RtOptions::new(wrap_width))
            };
        wrapped_rows += line_wrapped
            .iter()
            .map(|wrapped_line| wrapped_line.width().max(1).div_ceil(wrap_width))
            .sum::<usize>();
        wrapped.extend(line_wrapped);
    }
    let wrapped_lines = u16::try_from(wrapped_rows).unwrap_or(u16::MAX);
    with_unpinned_scrollback(terminal, reserved, |writer| {
        queue!(writer, MoveTo(0, area.top() - reserved))?;
        for (index, line) in wrapped.iter().enumerate() {
            if index > 0 {
                queue!(writer, Print("\r\n"))?;
            }
            write_history_line(writer, line, wrap_width)?;
        }

        // Make room for the composer and the header before repinning it. Only
        // full-screen line feeds reliably retain history across terminals.
        for _ in 0..area.height.saturating_add(reserved) {
            queue!(writer, Print("\r\n"), Clear(ClearType::UntilNewLine))?;
        }
        Ok(())
    })?;

    area.y = area
        .top()
        .saturating_add(wrapped_lines)
        .min(screen_size.height.saturating_sub(area.height));
    if area != terminal.viewport_area {
        terminal.set_viewport_area(area);
    }
    terminal.note_history_rows_inserted(wrapped_lines);
    Ok(())
}

/// Make room for a growing viewport without discarding the oldest history.
/// The caller must reposition/redraw the viewport and repaint reserved chrome.
pub(crate) fn scroll_history_up<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    scroll_by: u16,
    top_reserved_rows: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let screen_size = terminal.size()?;
    if scroll_by == 0 || screen_size.width == 0 || screen_size.height == 0 {
        return Ok(());
    }
    let reserved = top_reserved_rows.min(terminal.viewport_area.top());
    with_unpinned_scrollback(terminal, reserved, |writer| {
        queue!(writer, MoveTo(0, screen_size.height - 1))?;
        for _ in 0..scroll_by {
            queue!(writer, Print("\r\n"))?;
        }
        Ok(())
    })
}

/// Remove chrome, operate on the full screen, then restore space for chrome.
/// A nonzero DECSTBM top margin discards history instead of saving scrollback;
/// even a zero top margin with a partial bottom margin is unreliable (WezTerm).
fn with_unpinned_scrollback<B>(
    terminal: &mut crate::custom_terminal::Terminal<B>,
    reserved: u16,
    write_history: impl FnOnce(&mut B) -> io::Result<()>,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let cursor = terminal.last_known_cursor_pos;
    queue!(
        terminal.backend_mut(),
        ResetScrollRegion,
        SetAttribute(crossterm::style::Attribute::Reset),
        SetColors(Colors::new(CColor::Reset, CColor::Reset))
    )?;
    // Clear the composer before any scrolling can push it into history, and
    // invalidate its diff baseline even if its geometry will stay unchanged.
    terminal.clear()?;
    let writer = terminal.backend_mut();
    if reserved > 0 {
        queue!(writer, MoveTo(0, 0))?;
        write!(writer, "\x1b[{reserved}M")?; // DL: remove the header, not history.
    }
    write_history(writer)?;
    if reserved > 0 {
        queue!(writer, MoveTo(0, 0))?;
        write!(writer, "\x1b[{reserved}L")?; // IL: restore blank header rows.
    }
    queue!(writer, MoveTo(cursor.x, cursor.y))?;
    Ok(())
}

fn write_history_line(
    writer: &mut impl Write,
    line: &Line<'_>,
    wrap_width: usize,
) -> io::Result<()> {
    // URL lines can be wider than the terminal and character-wrap onto
    // continuation rows. Clear them so stale text is not left behind.
    let physical_rows = line.width().max(1).div_ceil(wrap_width);
    if physical_rows > 1 {
        queue!(writer, SavePosition)?;
        for _ in 1..physical_rows {
            queue!(writer, MoveDown(1), MoveToColumn(0))?;
            queue!(writer, Clear(ClearType::UntilNewLine))?;
        }
        queue!(writer, RestorePosition)?;
    }
    queue!(
        writer,
        SetColors(Colors::new(
            line.style
                .fg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset),
            line.style
                .bg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset)
        )),
        Clear(ClearType::UntilNewLine)
    )?;
    let merged_spans: Vec<Span> = line
        .spans
        .iter()
        .map(|s| Span {
            style: s.style.patch(line.style),
            content: s.content.clone(),
        })
        .collect();
    write_spans(writer, merged_spans.iter())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }
}

pub fn write_spans<'a, I>(mut writer: &mut impl Write, content: I) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut last_modifier = Modifier::empty();
    let mut current_link: Option<String> = None;
    let osc8_enabled = crate::osc8::enabled();
    for span in content {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            let diff = ModifierDiff {
                from: last_modifier,
                to: modifier,
            };
            diff.queue(&mut writer)?;
            last_modifier = modifier;
        }
        let next_fg = span.style.fg.unwrap_or(Color::Reset);
        let next_bg = span.style.bg.unwrap_or(Color::Reset);
        if next_fg != fg || next_bg != bg {
            queue!(
                writer,
                SetColors(Colors::new(
                    next_fg.into_crossterm(),
                    next_bg.into_crossterm()
                ))
            )?;
            fg = next_fg;
            bg = next_bg;
        }

        let next_link = if osc8_enabled {
            span.style.underline_color.and_then(crate::osc8::lookup)
        } else {
            None
        };
        if next_link != current_link {
            if current_link.is_some() {
                queue!(writer, Print(crate::osc8::close()))?;
            }
            if let Some(ref url) = next_link {
                queue!(writer, Print(crate::osc8::open(url)))?;
            }
            current_link = next_link;
        }

        queue!(writer, Print(span.content.clone()))?;
    }

    if current_link.is_some() {
        queue!(writer, Print(crate::osc8::close()))?;
    }

    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )
}

#[cfg(test)]
pub(crate) mod tests;
