//! Chaos terminal theme — green phosphor CRT aesthetic.
//!
//! All UI colors flow through this module. Swap the palette here to reskin
//! the entire terminal.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};

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
    bg: Color::Black,
    fg: Color::LightGreen,
    dim: Color::Green,
    highlight: Color::LightGreen,
    user_msg_bg: Color::DarkGray,
    border: Color::Green,
    warning: Color::Yellow,
    error: Color::LightRed,
    success: Color::LightGreen,
    accent: Color::LightGreen,
};

/// Anthropic orange palette — used when clamped to Claude Code MAX.
pub const ANTHROPIC: Palette = Palette {
    bg: Color::Black,
    fg: Color::LightYellow, // warm ANSI approximation
    dim: Color::Yellow,     // muted ANSI approximation
    highlight: Color::Yellow,
    user_msg_bg: Color::DarkGray,
    border: Color::Yellow,
    warning: Color::Yellow,
    error: Color::LightRed,
    success: Color::LightYellow,
    accent: Color::Yellow,
};

/// Active palette. Switches to Anthropic orange when clamped.
pub fn palette() -> &'static Palette {
    if is_clamped() { &ANTHROPIC } else { &PHOSPHOR }
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
    Style::default().fg(palette().fg).bg(palette().border)
}

/// Scanline effect — alternating row dimming for CRT feel.
/// Apply to even-numbered rows for the phosphor scanline look.
pub fn scanline(row: u16) -> Style {
    if row.is_multiple_of(2) {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}
