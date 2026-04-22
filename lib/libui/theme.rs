//! Chaos terminal theme — green phosphor CRT aesthetic.
//!
//! Theme selection and semantic slot layout now live in `chaos-chassis`; this
//! module remains the ratatui adapter used by the TUI.
#![allow(dead_code)]

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use chaos_chassis::theme::ThemeFamily;
use chaos_chassis::theme::ToneToken;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

/// Global clamped-mode flag — when true, the theme switches to Anthropic orange.
static CLAMPED: AtomicBool = AtomicBool::new(false);

/// Set clamped mode (switches theme to Anthropic orange).
pub fn set_clamped(clamped: bool) {
    CLAMPED.store(clamped, Ordering::Relaxed);
}

/// Whether the UI is in clamped mode.
pub fn is_clamped() -> bool {
    CLAMPED.load(Ordering::Relaxed)
}

/// The Chaos terminal palette. Every color used in the TUI should come from
/// here so the theme can be swapped in one place.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub highlight: Color,
    pub user_msg_bg: Color,
    pub border: Color,
    pub warning: Color,
    pub error: Color,
    pub success: Color,
    pub accent: Color,
}

fn map_tone(token: ToneToken) -> Color {
    match token {
        ToneToken::Black => Color::Black,
        ToneToken::LightGreen => Color::LightGreen,
        ToneToken::Green => Color::Green,
        ToneToken::DarkGray => Color::DarkGray,
        ToneToken::Yellow => Color::Yellow,
        ToneToken::LightRed => Color::LightRed,
        ToneToken::Cyan => Color::Cyan,
        ToneToken::WarmOrange => Color::LightYellow,
        ToneToken::Amber => Color::Yellow,
        ToneToken::DarkGreenBg => Color::DarkGray,
        ToneToken::DarkAmberBg => Color::DarkGray,
    }
}

fn theme_family() -> ThemeFamily {
    if is_clamped() {
        ThemeFamily::Anthropic
    } else {
        ThemeFamily::Phosphor
    }
}

/// Active palette. Switches to Anthropic orange when clamped.
pub fn palette() -> Palette {
    let palette = theme_family().tokens().map(map_tone);
    Palette {
        bg: palette.bg,
        fg: palette.fg,
        dim: palette.dim,
        highlight: palette.highlight,
        user_msg_bg: palette.user_msg_bg,
        border: palette.border,
        warning: palette.warning,
        error: palette.error,
        success: palette.success,
        accent: palette.accent,
    }
}

/// Default base style — green-on-black.
pub fn base() -> Style {
    let palette = palette();
    Style::default().fg(palette.fg).bg(palette.bg)
}

/// Dimmed text (secondary info, timestamps, metadata).
pub fn dim() -> Style {
    Style::default().fg(palette().dim)
}

/// Highlighted / selected element.
pub fn highlight() -> Style {
    Style::default()
        .fg(palette().highlight)
        .add_modifier(Modifier::BOLD)
}

/// Border / chrome style.
pub fn border() -> Style {
    Style::default().fg(palette().border)
}

/// User-authored message background.
pub fn user_message() -> Style {
    let palette = palette();
    Style::default().fg(palette.fg).bg(palette.user_msg_bg)
}

/// Warning text (amber phosphor).
pub fn warning() -> Style {
    Style::default().fg(palette().warning)
}

/// Error text (red phosphor).
pub fn error() -> Style {
    Style::default()
        .fg(palette().error)
        .add_modifier(Modifier::BOLD)
}

/// Success / approved text.
pub fn success() -> Style {
    Style::default().fg(palette().success)
}

/// Input prompt accent (the `>` cursor).
pub fn prompt() -> Style {
    Style::default()
        .fg(palette().accent)
        .add_modifier(Modifier::BOLD)
}

/// Replaces `Color::Cyan` — used for links, interactive elements.
pub fn cyan() -> Color {
    palette().accent
}

/// Replaces `Color::Green` — used for success, additions, positive states.
pub fn green() -> Color {
    palette().success
}

/// Replaces `Color::Red` — used for errors, deletions, negative states.
pub fn red() -> Color {
    palette().error
}

/// Replaces `Color::LightBlue` — used for code blocks, inline code.
pub fn light_blue() -> Color {
    palette().dim
}

/// Replaces `Color::Yellow` — used for warnings, caution states.
pub fn yellow() -> Color {
    palette().warning
}

/// Diff addition line style — green on dark green background.
pub fn diff_add() -> Style {
    Style::default().fg(Color::Black).bg(palette().success)
}

/// Diff deletion line style — red on dark red background.
pub fn diff_del() -> Style {
    Style::default().fg(Color::Black).bg(palette().error)
}

/// Diff context / unchanged line style.
pub fn diff_context() -> Style {
    Style::default().fg(palette().dim)
}

/// Key hint style — dim green for shortcut labels.
pub fn key_hint() -> Style {
    Style::default()
        .fg(palette().dim)
        .add_modifier(Modifier::DIM)
}

/// Status bar / footer style.
pub fn status_bar() -> Style {
    let palette = palette();
    Style::default().fg(palette.fg).bg(palette.border)
}

/// Scanline effect — alternating row dimming for CRT feel.
pub fn scanline(row: u16) -> Style {
    if row.is_multiple_of(2) {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}
