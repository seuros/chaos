//! Chaos terminal theme — mode-aware terminal palettes.
//!
//! Theme selection and semantic slot layout now live in `chaos-chassis`; this
//! module remains the ratatui adapter used by the TUI.
#![allow(dead_code)]

use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use chaos_ipc::config_types::ModeKind;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

/// Global clamped-mode flag — when true, the theme switches to Anthropic orange.
static CLAMPED: AtomicBool = AtomicBool::new(false);
/// Global collaboration-mode tint used for chrome rendering (top bar + borders).
static COLLABORATION_MODE: AtomicU8 = AtomicU8::new(collaboration_mode_to_u8(ModeKind::Default));

/// Set clamped mode (switches theme to Anthropic orange).
pub fn set_clamped(clamped: bool) {
    CLAMPED.store(clamped, Ordering::Relaxed);
}

/// Whether the UI is in clamped mode.
pub fn is_clamped() -> bool {
    CLAMPED.load(Ordering::Relaxed)
}

const fn collaboration_mode_to_u8(mode: ModeKind) -> u8 {
    match mode {
        ModeKind::Plan => 1,
        ModeKind::Default | ModeKind::PairProgramming | ModeKind::Execute => 0,
    }
}

fn collaboration_mode_from_u8(value: u8) -> ModeKind {
    match value {
        1 => ModeKind::Plan,
        _ => ModeKind::Default,
    }
}

/// Set the collaboration mode used for theme tinting.
pub fn set_collaboration_mode(mode: ModeKind) {
    COLLABORATION_MODE.store(collaboration_mode_to_u8(mode), Ordering::Relaxed);
}

/// Returns the collaboration mode currently used for theme tinting.
pub fn collaboration_mode() -> ModeKind {
    collaboration_mode_from_u8(COLLABORATION_MODE.load(Ordering::Relaxed))
}

/// The Chaos terminal palette. Every color used in the TUI should come from
/// here so the theme can be swapped in one place.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub highlight: Color,
    pub top_bar_bg: Color,
    pub top_bar_fg: Color,
    pub top_bar_dim: Color,
    pub user_msg_bg: Color,
    pub border: Color,
    pub warning: Color,
    pub error: Color,
    pub success: Color,
    pub accent: Color,
    pub secondary_accent: Color,
    pub tertiary_accent: Color,
}

fn execution_palette() -> Palette {
    if is_clamped() {
        Palette {
            bg: Color::Black,
            fg: Color::White,
            dim: Color::Gray,
            highlight: Color::LightYellow,
            top_bar_bg: Color::DarkGray,
            top_bar_fg: Color::White,
            top_bar_dim: Color::Gray,
            user_msg_bg: Color::Black,
            border: Color::Yellow,
            warning: Color::Yellow,
            error: Color::LightRed,
            success: Color::LightYellow,
            accent: Color::Yellow,
            secondary_accent: Color::Magenta,
            tertiary_accent: Color::Gray,
        }
    } else {
        Palette {
            bg: Color::Black,
            fg: Color::White,
            dim: Color::Gray,
            highlight: Color::Cyan,
            top_bar_bg: Color::DarkGray,
            top_bar_fg: Color::White,
            top_bar_dim: Color::Gray,
            user_msg_bg: Color::Black,
            border: Color::Blue,
            warning: Color::Yellow,
            error: Color::LightRed,
            success: Color::Green,
            accent: Color::Cyan,
            secondary_accent: Color::Blue,
            tertiary_accent: Color::Gray,
        }
    }
}

fn plan_palette() -> Palette {
    Palette {
        bg: Color::Black,
        fg: Color::LightGreen,
        dim: Color::Green,
        highlight: Color::LightGreen,
        top_bar_bg: Color::DarkGray,
        top_bar_fg: Color::White,
        top_bar_dim: Color::Gray,
        user_msg_bg: Color::Black,
        border: Color::LightGreen,
        warning: Color::Yellow,
        error: Color::LightRed,
        success: Color::Green,
        accent: Color::Cyan,
        secondary_accent: Color::Magenta,
        tertiary_accent: Color::Blue,
    }
}

fn default_badge_bg() -> Color {
    if is_clamped() {
        Color::Yellow
    } else {
        Color::Blue
    }
}

fn plan_badge_bg() -> Color {
    Color::Green
}

/// Active palette. Switches to Anthropic orange when clamped.
pub fn palette() -> Palette {
    match collaboration_mode() {
        ModeKind::Plan => plan_palette(),
        ModeKind::Default | ModeKind::PairProgramming | ModeKind::Execute => execution_palette(),
    }
}

/// Default base style — mode-aware foreground on black.
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

/// Warning text.
pub fn warning() -> Style {
    Style::default().fg(palette().warning)
}

/// Error text.
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

/// Primary interactive accent — used for links, identifiers, and selected labels.
pub fn accent_color() -> Color {
    palette().accent
}

/// Positive accent — used for success, additions, and approved states.
pub fn success_color() -> Color {
    palette().success
}

/// Negative accent — used for errors, deletions, and failed states.
pub fn error_color() -> Color {
    palette().error
}

/// Muted foreground color — used for low-emphasis markers and secondary inline text.
pub fn dim_color() -> Color {
    palette().dim
}

/// Warning accent — used for cautionary states and warnings.
pub fn warning_color() -> Color {
    palette().warning
}

/// Secondary accent — used for annotations, slash commands, and auxiliary metadata.
pub fn annotation_color() -> Color {
    palette().secondary_accent
}

/// Tertiary accent — used for contained / closed-world labels and similar bounded states.
pub fn contained_color() -> Color {
    palette().tertiary_accent
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

/// Key hint style — muted mode color for shortcut labels.
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

/// Style for the collaboration-mode badge shown in footer chrome.
pub fn collaboration_mode_badge(mode: ModeKind) -> Style {
    match mode {
        ModeKind::Plan => Style::default()
            .fg(Color::White)
            .bg(plan_badge_bg())
            .add_modifier(Modifier::BOLD),
        ModeKind::Default | ModeKind::PairProgramming | ModeKind::Execute => Style::default()
            .fg(Color::Black)
            .bg(default_badge_bg())
            .add_modifier(Modifier::BOLD),
    }
}

/// Scanline effect — alternating row dimming for CRT feel.
pub fn scanline(row: u16) -> Style {
    if row.is_multiple_of(2) {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}
