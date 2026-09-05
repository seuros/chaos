//! Responsive, single-row terminal chrome.
//!
//! Widgets own cached presentation and refresh deadlines. The runtime updates
//! them independently of drawing; the layout admits whole widgets by priority.
//! Built-ins cover machine identity, environment, persistence, power, and time.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;

mod layout;
mod runtime;
mod widget;
mod widgets;

pub(crate) use runtime::Runtime;
use widget::{BarWidget, Content, Tone};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
struct WidgetSpec {
    id: &'static str,
    side: Side,
    priority: u8,
    min_width: usize,
}

#[derive(Default)]
struct Update {
    changed: bool,
    next: Option<Duration>,
}

struct Bar<'a> {
    widgets: &'a [BarWidget],
    palette: crate::theme::Palette,
}

impl WidgetRef for Bar<'_> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let area = Rect {
            height: area.height.min(1),
            ..area
        };
        let base = Style::default()
            .fg(self.palette.top_bar_fg)
            .bg(self.palette.top_bar_bg);
        // Background coverage is independent of which widgets fit.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].reset();
                buf[(x, y)].set_style(base);
            }
        }

        let specs: Vec<_> = self.widgets.iter().map(BarWidget::spec).collect();
        for placement in layout::arrange(area, &specs) {
            if let Some(separator) = placement.separator {
                Line::from(" │ ")
                    .style(Style::default().fg(self.palette.top_bar_dim))
                    .render(separator, buf);
            }
            self.widgets[placement.index].render(placement.area, buf, self.palette);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
