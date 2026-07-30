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
use super::XaiDeviceCodeLoginState;
use super::mark_url_hyperlink;

struct DeviceCodeView<'a> {
    verification_url: &'a str,
    user_code: &'a str,
    expires_in_minutes: u64,
}

pub(super) fn render_device_code_login(
    widget: &AccountsWidget,
    area: Rect,
    buf: &mut Buffer,
    state: &ContinueWithDeviceCodeState,
) {
    let view = state
        .device_code
        .as_ref()
        .map(|device_code| DeviceCodeView {
            verification_url: device_code.verification_url.as_str(),
            user_code: device_code.user_code.as_str(),
            expires_in_minutes: 15,
        });
    render_device_code(widget, area, buf, view);
}

pub(super) fn render_xai_device_code_login(
    widget: &AccountsWidget,
    area: Rect,
    buf: &mut Buffer,
    state: &XaiDeviceCodeLoginState,
) {
    let view = state
        .device_code
        .as_ref()
        .map(|device_code| DeviceCodeView {
            verification_url: device_code.verification_url.as_str(),
            user_code: device_code.user_code.as_str(),
            expires_in_minutes: device_code.expires_in.div_ceil(60),
        });
    render_device_code(widget, area, buf, view);
}

fn render_device_code(
    widget: &AccountsWidget,
    area: Rect,
    buf: &mut Buffer,
    view: Option<DeviceCodeView<'_>>,
) {
    let banner = if view.is_some() {
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

    let verification_url = if let Some(view) = &view {
        lines.push("  1. Open this link in your browser and sign in".into());
        lines.push("".into());
        lines.push(Line::from(vec![
            "  ".into(),
            view.verification_url.cyan().underlined(),
        ]));
        lines.push("".into());
        lines.push(
            format!(
                "  2. Enter this one-time code after you are signed in (expires in {} minutes)",
                view.expires_in_minutes
            )
            .into(),
        );
        lines.push("".into());
        lines.push(Line::from(vec!["  ".into(), view.user_code.cyan().bold()]));
        lines.push("".into());
        lines.push(
            "  Device codes are a common phishing target. Never share this code."
                .dim()
                .into(),
        );
        lines.push("".into());
        Some(view.verification_url.to_string())
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
