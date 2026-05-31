use unicode_width::UnicodeWidthStr;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FooterTip {
    pub(super) text: String,
    pub(super) highlight: bool,
}

impl FooterTip {
    pub(super) fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            highlight: false,
        }
    }

    pub(super) fn highlighted(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            highlight: true,
        }
    }
}

pub(super) fn wrap_footer_tips(
    width: u16,
    separator: &str,
    tips: Vec<FooterTip>,
) -> Vec<Vec<FooterTip>> {
    let max_width = width.max(1) as usize;
    let separator_width = UnicodeWidthStr::width(separator);
    if tips.is_empty() {
        return vec![Vec::new()];
    }

    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut used = 0usize;

    for tip in tips {
        let tip_width = UnicodeWidthStr::width(tip.text.as_str()).min(max_width);
        let extra = if current.is_empty() {
            tip_width
        } else {
            separator_width.saturating_add(tip_width)
        };
        if !current.is_empty() && used.saturating_add(extra) > max_width {
            lines.push(current);
            current = Vec::new();
            used = 0;
        }
        if current.is_empty() {
            used = tip_width;
        } else {
            used = used
                .saturating_add(separator_width)
                .saturating_add(tip_width);
        }
        current.push(tip);
    }

    if current.is_empty() {
        lines.push(Vec::new());
    } else {
        lines.push(current);
    }
    lines
}

pub(super) fn render_footer_tip_lines<F>(
    area: Rect,
    buf: &mut Buffer,
    tip_lines: Vec<Vec<FooterTip>>,
    separator: &str,
    truncate: Option<F>,
) where
    F: Fn(Line<'static>, usize) -> Line<'static>,
{
    if area.is_empty() {
        return;
    }

    for (row_idx, tips) in tip_lines.into_iter().take(area.height as usize).enumerate() {
        let mut spans = Vec::new();
        for (tip_idx, tip) in tips.into_iter().enumerate() {
            if tip_idx > 0 {
                spans.push(separator.to_string().into());
            }
            if tip.highlight {
                spans.push(tip.text.fg(crate::theme::accent_color()).bold().not_dim());
            } else {
                spans.push(tip.text.into());
            }
        }
        let line = Line::from(spans).dim();
        let line = match &truncate {
            Some(truncate) => truncate(line, area.width as usize),
            None => line,
        };
        Paragraph::new(line).render(
            Rect {
                x: area.x,
                y: area.y.saturating_add(row_idx as u16),
                width: area.width,
                height: 1,
            },
            buf,
        );
    }
}
