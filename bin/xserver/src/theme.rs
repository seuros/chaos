//! Chaos terminal theme — green phosphor CRT aesthetic.
//!
//! All UI colors flow through this module. Swap the palette here to reskin
//! the entire terminal.
#![allow(dead_code)]

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

/// The Chaos terminal palette. Every color used in the TUI should come from
/// here so the theme can be swapped in one place.
pub struct Palette {
    /// Terminal background — near-black with a green tint.
    pub bg: Color,
    /// Primary text — bright phosphor green.
    pub fg: Color,
    /// Dimmed/secondary text — dark green.
    pub dim: Color,
    /// Highlighted/selected elements — bright green.
    pub highlight: Color,
    /// User message background — subtle green tint.
    pub user_msg_bg: Color,
    /// Borders and chrome — muted green.
    pub border: Color,
    /// Warning/caution — amber phosphor.
    pub warning: Color,
    /// Error/danger — red phosphor.
    pub error: Color,
    /// Success/approved — brighter green.
    pub success: Color,
    /// Input prompt accent.
    pub accent: Color,
}

/// Default Chaos green phosphor palette.
pub const PHOSPHOR: Palette = Palette {
    bg: Color::Rgb(10, 15, 10),
    fg: Color::Rgb(51, 255, 51),
    dim: Color::Rgb(26, 138, 26),
    highlight: Color::Rgb(85, 255, 85),
    user_msg_bg: Color::Rgb(15, 30, 15),
    border: Color::Rgb(13, 94, 13),
    warning: Color::Rgb(255, 183, 51),
    error: Color::Rgb(255, 51, 51),
    success: Color::Rgb(51, 255, 51),
    accent: Color::Rgb(51, 255, 51),
};

/// Active palette. Change this to swap the entire theme.
pub fn palette() -> &'static Palette {
    &PHOSPHOR
}

// --- Semantic styles built from the palette ---

/// Default base style — green-on-black.
pub fn base() -> Style {
    Style::default().fg(palette().fg).bg(palette().bg)
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
    Style::default().fg(palette().fg).bg(palette().user_msg_bg)
}

/// Warning text (amber phosphor).
pub fn warning() -> Style {
    Style::default().fg(palette().warning)
}

/// Error text (red phosphor).
pub fn error() -> Style {
    Style::default().fg(palette().error).add_modifier(Modifier::BOLD)
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

// --- Color aliases for gradual migration from hardcoded Color::* ---
// Use these instead of Color::Cyan, Color::Green, etc. throughout the TUI.
// Each maps a semantic role to the palette.

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
    Style::default()
        .fg(palette().success)
        .bg(Color::Rgb(10, 30, 10))
}

/// Diff deletion line style — red on dark red background.
pub fn diff_del() -> Style {
    Style::default()
        .fg(palette().error)
        .bg(Color::Rgb(30, 10, 10))
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
    Style::default().fg(palette().fg).bg(palette().border)
}

/// Scanline effect — alternating row dimming for CRT feel.
/// Apply to even-numbered rows for the phosphor scanline look.
pub fn scanline(row: u16) -> Style {
    if row % 2 == 0 {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}
