//! Top status bar showing system information at a glance.
//!
//! Renders a single-line bar at the top of the terminal with hostname,
//! OS/distro, battery status, current time, and timezone.  All data
//! comes from the [`chaos_sysinfo`] crate.
//!
//! The bar is rendered directly to the terminal (outside the ratatui
//! viewport) by [`crate::tui`] so that it stays pinned at screen row 0
//! while history scrolls beneath it.

use chaos_sysinfo::{SandboxKind, SystemInfo, sysinfo};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Build the top bar [`Line`] for direct terminal rendering (outside viewport).
pub fn top_bar_line(width: u16) -> Line<'static> {
    build_status_line(sysinfo(), width, crate::theme::palette())
}

fn build_status_line(
    info: &SystemInfo,
    width: u16,
    palette: crate::theme::Palette,
) -> Line<'static> {
    let bar_bg = palette.top_bar_bg;
    let sep = Span::styled(" │ ", Style::default().fg(palette.top_bar_dim).bg(bar_bg));
    let base = Style::default().fg(palette.top_bar_fg).bg(bar_bg);

    let mut spans: Vec<Span<'static>> = Vec::new();

    // Left side: hostname + OS
    spans.push(Span::styled(
        format!(" {}", info.hostname),
        base.add_modifier(Modifier::BOLD),
    ));

    spans.push(sep.clone());

    // OS + distro
    let os_label = if !info.os_distro.is_empty() {
        format!("{} ({})", info.os, info.os_distro)
    } else {
        info.os.clone()
    };
    spans.push(Span::styled(os_label, base));

    spans.push(sep.clone());

    // Arch
    spans.push(Span::styled(info.arch.clone(), base));

    // Sandbox
    if info.sandbox_type != SandboxKind::None {
        spans.push(sep.clone());
        let sandbox_label = match info.sandbox_type {
            SandboxKind::Seatbelt => "seatbelt",
            SandboxKind::Seccomp => "seccomp",
            SandboxKind::Capsicum => "capsicum",
            SandboxKind::None => unreachable!(),
        };
        spans.push(Span::styled(sandbox_label, base));
    }

    // Container
    if info.in_container {
        spans.push(sep.clone());
        let label = if info.container_type.is_empty() {
            "container".into()
        } else {
            info.container_type.clone()
        };
        spans.push(Span::styled(
            label,
            Style::default().fg(palette.warning).bg(bar_bg),
        ));
    }

    // Multiplexer — stable id only, so the label never goes stale when
    // neighbouring panes/windows are closed and the multiplexer renumbers
    // the visible coordinates.
    if let Some(ref mux) = info.multiplexer {
        spans.push(sep.clone());
        let label = if mux.id.is_empty() {
            mux.kind.clone()
        } else {
            format!("{} {}", mux.kind, mux.id)
        };
        spans.push(Span::styled(
            label,
            Style::default().fg(palette.accent).bg(bar_bg),
        ));
    }

    // Right side: battery + time
    let mut right_spans: Vec<Span<'static>> = Vec::new();

    // Battery
    if info.has_battery {
        let level = info.battery_level.unwrap_or(0);
        let (icon, color) = if info.charger_connected {
            ("⚡", palette.success)
        } else if level <= 15 {
            ("▼", palette.error)
        } else if level <= 30 {
            ("▽", palette.warning)
        } else {
            ("●", palette.success)
        };
        right_spans.push(Span::styled(
            format!("{icon} {level}%"),
            Style::default().fg(color).bg(bar_bg),
        ));
        right_spans.push(sep.clone());
    }

    // Current time
    let now = chrono_now();
    right_spans.push(Span::styled(now, base));
    right_spans.push(Span::styled(" ", base));

    // Calculate right-side width to right-align
    let right_width: u16 = right_spans.iter().map(|s| s.content.len() as u16).sum();

    let left_width: u16 = spans.iter().map(|s| s.content.len() as u16).sum();

    // Fill gap between left and right
    let gap = (width as i32) - (left_width as i32) - (right_width as i32);
    if gap > 0 {
        spans.push(Span::styled(
            " ".repeat(gap as usize),
            Style::default().bg(bar_bg),
        ));
    }

    spans.extend(right_spans);

    Line::from(spans)
}

/// Get current local time as HH:MM.
fn chrono_now() -> String {
    // Use libc to avoid pulling in chrono
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) };

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&tv.tv_sec, &mut tm) };

    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::config_types::ModeKind;

    #[test]
    fn top_bar_uses_dedicated_top_bar_background() {
        let mut info = sysinfo().clone();
        info.hostname = "host".into();
        info.os = "linux".into();
        info.os_distro = "arch".into();
        info.arch = "x86_64".into();
        info.has_battery = true;
        info.battery_level = Some(87);
        info.charger_connected = false;
        info.in_container = true;
        info.container_type = "podman".into();
        info.multiplexer = Some(chaos_sysinfo::MultiplexerInfo {
            kind: "tmux".into(),
            id: "%1".into(),
        });

        let palette = crate::theme::palette();
        let line = build_status_line(&info, 120, palette);

        for span in line.spans {
            assert_eq!(span.style.bg, Some(palette.top_bar_bg));
        }
    }

    #[test]
    fn top_bar_uses_white_text_and_gray_separator_tokens() {
        let mut info = sysinfo().clone();
        info.hostname = "host".into();
        info.os = "linux".into();
        info.os_distro.clear();
        info.arch = "x86_64".into();
        info.has_battery = false;
        info.in_container = false;
        info.multiplexer = None;

        let palette = crate::theme::palette();
        let line = build_status_line(&info, 80, palette);

        assert_eq!(line.spans[0].style.fg, Some(palette.top_bar_fg));
        assert_eq!(line.spans[1].style.fg, Some(palette.top_bar_dim));
    }

    #[test]
    fn top_bar_tint_changes_with_collaboration_mode() {
        let default_palette =
            crate::theme::palette_for_mode(ModeKind::Default, /*clamped*/ false);
        let plan_palette = crate::theme::palette_for_mode(ModeKind::Plan, /*clamped*/ false);

        assert_ne!(default_palette.top_bar_bg, plan_palette.top_bar_bg);
        assert_ne!(default_palette.border, plan_palette.border);
    }
}
