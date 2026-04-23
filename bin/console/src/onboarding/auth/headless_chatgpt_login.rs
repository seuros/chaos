use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

use crate::shimmer::shimmer_spans;

use super::AccountsWidget;
use super::ContinueWithDeviceCodeState;
use super::mark_url_hyperlink;

pub(super) fn render_device_code_login(
    widget: &AccountsWidget,
    area: Rect,
    buf: &mut Buffer,
    state: &ContinueWithDeviceCodeState,
) {
    let banner = if state.device_code.is_some() {
        "Finish signing in via your browser"
    } else {
        "Preparing device code login"
    };

    let mut spans = vec!["  ".into()];
    if widget.animations_enabled {
        widget
            .request_frame
            .schedule_frame_in(std::time::Duration::from_millis(100));
        spans.extend(shimmer_spans(banner));
    } else {
        spans.push(banner.into());
    }

    let mut lines = vec![spans.into(), "".into()];

    let verification_url = if let Some(device_code) = &state.device_code {
        lines.push("  1. Open this link in your browser and sign in".into());
        lines.push("".into());
        lines.push(Line::from(vec![
            "  ".into(),
            device_code.verification_url.as_str().cyan().underlined(),
        ]));
        lines.push("".into());
        lines.push(
            "  2. Enter this one-time code after you are signed in (expires in 15 minutes)".into(),
        );
        lines.push("".into());
        lines.push(Line::from(vec![
            "  ".into(),
            device_code.user_code.as_str().cyan().bold(),
        ]));
        lines.push("".into());
        lines.push(
            "  Device codes are a common phishing target. Never share this code."
                .dim()
                .into(),
        );
        lines.push("".into());
        Some(device_code.verification_url.clone())
    } else {
        lines.push("  Requesting a one-time code...".dim().into());
        lines.push("".into());
        None
    };

    lines.push("  Press Esc to cancel".dim().into());
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(area, buf);

    if let Some(url) = &verification_url {
        mark_url_hyperlink(buf, area, url);
    }
}
