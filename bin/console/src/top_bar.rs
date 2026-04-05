//! Top status bar showing system information at a glance.
//!
//! Renders a single-line bar at the top of the terminal with hostname,
//! OS/distro, battery status, current time, and timezone.  All data
//! comes from the kernel's [`chaos_kern::sysinfo`] module.
//!
//! The bar is rendered directly to the terminal (outside the ratatui
//! viewport) by [`crate::tui`] so that it stays pinned at screen row 0
//! while history scrolls beneath it.

use chaos_kern::sysinfo::{SandboxKind, SystemInfo, sysinfo};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Build the top bar [`Line`] for direct terminal rendering (outside viewport).
pub(crate) fn top_bar_line(width: u16) -> Line<'static> {
    build_status_line(sysinfo(), width)
}

fn build_status_line(info: &SystemInfo, width: u16) -> Line<'static> {
    let sep = Span::styled(" │ ", Style::default().fg(Color::Gray).bg(Color::DarkGray));
    let base = Style::default().fg(Color::White).bg(Color::DarkGray);

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
            Style::default().fg(Color::Yellow).bg(Color::DarkGray),
        ));
    }

    // Multiplexer
    if let Some(ref mux) = info.multiplexer {
        spans.push(sep.clone());
        let label = if mux.window.is_empty() {
            format!("{} {}:{}", mux.kind, mux.session, mux.pane)
        } else {
            format!("{} {}:{}.{}", mux.kind, mux.session, mux.window, mux.pane)
        };
        spans.push(Span::styled(
            label,
            Style::default().fg(Color::Cyan).bg(Color::DarkGray),
        ));
    }

    // Right side: battery + time
    let mut right_spans: Vec<Span<'static>> = Vec::new();

    // Battery
    if info.has_battery {
        let level = info.battery_level.unwrap_or(0);
        let (icon, color) = if info.charger_connected {
            ("⚡", Color::Green)
        } else if level <= 15 {
            ("▼", Color::Red)
        } else if level <= 30 {
            ("▽", Color::Yellow)
        } else {
            ("●", Color::Green)
        };
        right_spans.push(Span::styled(
            format!("{icon} {level}%"),
            Style::default().fg(color).bg(Color::DarkGray),
        ));
        right_spans.push(sep.clone());
    }

    // Current time
    let now = chrono_now();
    right_spans.push(Span::styled(now, base));
    right_spans.push(Span::raw(" "));

    // Calculate right-side width to right-align
    let right_width: u16 = right_spans.iter().map(|s| s.content.len() as u16).sum();

    let left_width: u16 = spans.iter().map(|s| s.content.len() as u16).sum();

    // Fill gap between left and right
    let gap = (width as i32) - (left_width as i32) - (right_width as i32);
    if gap > 0 {
        spans.push(Span::styled(
            " ".repeat(gap as usize),
            Style::default().bg(Color::DarkGray),
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
