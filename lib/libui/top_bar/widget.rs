//! Shared cached-text widget with static, timed, and snapshot-driven updates.
//!
//! Built-ins provide identity and presentation rules, not Ratatui plumbing.
//! Refresh callbacks must be nonblocking; the runtime owns I/O and wakeups.

use std::borrow::Cow;
use std::time::Duration;

use jiff::Zoned;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Widget;
use tokio::sync::watch;

use super::{Side, Update, WidgetSpec};
use crate::theme::Palette;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tone {
    #[default]
    Normal,
    Accent,
    Success,
    Warning,
    Error,
}

/// Semantic styling is resolved against the container's palette at draw time.
#[derive(Default, PartialEq, Eq)]
pub(super) struct Content {
    text: Cow<'static, str>,
    tone: Tone,
    bold: bool,
}

impl Content {
    pub(super) fn new(text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub(super) fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub(super) fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn style(&self, palette: Palette) -> Style {
        let color = match self.tone {
            Tone::Normal => palette.top_bar_fg,
            Tone::Accent => palette.accent,
            Tone::Success => palette.success,
            Tone::Warning => palette.warning,
            Tone::Error => palette.error,
        };
        Style::default().fg(color).add_modifier(if self.bold {
            Modifier::BOLD
        } else {
            Modifier::empty()
        })
    }
}

type Refresh = dyn FnMut(&mut Content, &Zoned) -> Update + Send + Sync;

pub(super) struct BarWidget {
    id: &'static str,
    side: Side,
    priority: u8,
    warning_priority: Option<u8>,
    content: Content,
    updater: Option<Box<Refresh>>,
}

impl BarWidget {
    pub(super) fn text(id: &'static str, side: Side, priority: u8, content: Content) -> Self {
        Self {
            id,
            side,
            priority,
            warning_priority: None,
            content,
            updater: None,
        }
    }

    /// Attach a cached-state update, never a timer or blocking data collector.
    pub(super) fn with_refresh(
        mut self,
        refresh: impl FnMut(&mut Content, &Zoned) -> Update + Send + Sync + 'static,
    ) -> Self {
        self.updater = Some(Box::new(refresh));
        self
    }

    /// Sample wall time on refresh and return the next delay to the shared timer.
    pub(super) fn timed(
        id: &'static str,
        side: Side,
        priority: u8,
        sample: fn(&Zoned) -> (Content, Duration),
    ) -> Self {
        Self::text(id, side, priority, Content::default()).with_refresh(move |cached, now| {
            let (content, delay) = sample(now);
            let changed = *cached != content;
            *cached = content;
            Update {
                changed,
                next: Some(delay),
            }
        })
    }

    /// Retain the initial snapshot and redraw only when presentation changes.
    /// Unrelated fields and sub-display precision changes do not cause redraws.
    /// The runtime wakes on notifications; this adapter creates no polling loop.
    pub(super) fn watched<T>(
        id: &'static str,
        side: Side,
        priority: u8,
        mut source: watch::Receiver<T>,
        present: impl Fn(&T) -> Content + Send + Sync + 'static,
    ) -> Self
    where
        T: Send + Sync + 'static,
    {
        let content = present(&source.borrow_and_update());
        Self::text(id, side, priority, content).with_refresh(move |cached, _| {
            let content = present(&source.borrow_and_update());
            let changed = *cached != content;
            if changed {
                *cached = content;
            }
            Update {
                changed,
                next: None,
            }
        })
    }

    pub(super) fn with_warning_priority(mut self, priority: u8) -> Self {
        self.warning_priority = Some(priority);
        self
    }

    pub(super) fn spec(&self) -> WidgetSpec {
        WidgetSpec {
            id: self.id,
            side: self.side,
            priority: if matches!(self.content.tone, Tone::Warning | Tone::Error) {
                self.warning_priority.unwrap_or(self.priority)
            } else {
                self.priority
            },
            min_width: crate::width::display_width(&self.content.text),
        }
    }

    pub(super) fn set_tone(&mut self, tone: Tone) {
        self.content.tone = tone;
    }

    pub(super) fn refresh(&mut self, now: &Zoned) -> Update {
        match &mut self.updater {
            Some(refresh) => refresh(&mut self.content, now),
            None => Update::default(),
        }
    }

    pub(super) fn render(&self, area: Rect, buf: &mut Buffer, palette: Palette) {
        Line::from(self.content.text.as_ref())
            .style(self.content.style(palette))
            .render(area, buf);
    }
}
