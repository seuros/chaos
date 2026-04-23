//! Shared test helpers and fixtures.
//!
//! The module is `pub` rather than `#[cfg(test)]`-gated because
//! `chatwidget::tests` is compiled as a non-test module under the `testing`
//! feature; gating this module breaks that build.
#![allow(dead_code)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::render::renderable::Renderable;
use crate::test_render::buffer_to_string;
use crate::test_render::render_to_first_char_string;
use crate::test_render::render_to_string;

/// Draw into a [`TestBackend`] terminal and return the snapshot string that
/// `insta` would record for the backend.
pub(crate) fn render_test_backend_debug(
    width: u16,
    height: u16,
    draw: impl FnOnce(&mut ratatui::Frame<'_>),
) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height))
        .unwrap_or_else(|err| panic!("create test terminal: {err}"));
    terminal
        .draw(draw)
        .unwrap_or_else(|err| panic!("draw test terminal: {err}"));
    terminal.backend().to_string()
}

/// Return the area for a renderable at the requested width and its desired height.
pub(crate) fn area_with_desired_height(renderable: &impl Renderable, width: u16) -> Rect {
    Rect::new(0, 0, width, renderable.desired_height(width))
}

/// Render a [`Renderable`] to a character-grid string.
pub(crate) fn renderable_first_char_string(renderable: &impl Renderable, area: Rect) -> String {
    render_to_first_char_string(renderable, area)
}

/// Render a [`Renderable`] into a [`Buffer`] for direct cell/style inspection in tests.
pub(crate) fn renderable_buffer<R: Renderable + ?Sized>(renderable: &R, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    renderable.render(area, &mut buf);
    buf
}

/// Render a [`Renderable`] into a [`Buffer`] for a fixed size.
pub(crate) fn renderable_buffer_with_size<R: Renderable + ?Sized>(
    renderable: &R,
    width: u16,
    height: u16,
) -> Buffer {
    renderable_buffer(renderable, Rect::new(0, 0, width, height))
}

/// Render a [`Renderable`] to a character-grid string at a fixed size.
pub(crate) fn renderable_first_char_string_with_size(
    renderable: &impl Renderable,
    width: u16,
    height: u16,
) -> String {
    renderable_first_char_string(renderable, Rect::new(0, 0, width, height))
}

/// Render a [`Renderable`] to a string while trimming trailing whitespace from
/// each rendered row.
pub(crate) fn renderable_trim_end_string(renderable: &impl Renderable, area: Rect) -> String {
    render_to_string(renderable, area)
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a [`Renderable`] to a string at a fixed size.
pub(crate) fn renderable_string_with_size<R: Renderable + ?Sized>(
    renderable: &R,
    width: u16,
    height: u16,
) -> String {
    buffer_to_string(&renderable_buffer_with_size(renderable, width, height))
}

/// Render a [`Renderable`] to a string while trimming trailing whitespace from
/// each rendered row at a fixed size.
pub(crate) fn renderable_trim_end_string_with_size(
    renderable: &impl Renderable,
    width: u16,
    height: u16,
) -> String {
    renderable_trim_end_string(renderable, Rect::new(0, 0, width, height))
}

/// Render a [`Renderable`] to a trimmed string at its desired height.
pub(crate) fn renderable_trim_end_string_at_desired_height(
    renderable: &impl Renderable,
    width: u16,
) -> String {
    renderable_trim_end_string(renderable, area_with_desired_height(renderable, width))
}

/// Convert a rendered buffer row into a plain string.
pub(crate) fn buffer_row_string(buf: &Buffer, row: u16) -> String {
    let area = buf.area();
    let mut line = String::new();
    for col in 0..area.width {
        let symbol = buf[(area.x + col, area.y + row)].symbol();
        if symbol.is_empty() {
            line.push(' ');
        } else {
            line.push_str(symbol);
        }
    }
    line
}

/// Convert rendered text lines into plain strings with span content concatenated.
pub(crate) fn plain_line_strings(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// Convert a rendered buffer into per-row strings.
pub(crate) fn buffer_lines(buf: &Buffer) -> Vec<String> {
    buffer_to_string(buf)
        .lines()
        .map(ToString::to_string)
        .collect()
}

/// Create an AppEventSender for tests. The receiver is dropped.
pub(crate) fn make_app_event_sender() -> AppEventSender {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    AppEventSender::new(tx)
}

/// Create an AppEventSender for tests along with the receiver for asserting events.
pub(crate) fn make_app_event_sender_with_rx() -> (AppEventSender, UnboundedReceiver<AppEvent>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    (AppEventSender::new(tx), rx)
}
