//! Shared types, constants, and style helpers for diff rendering.
//!
//! This module owns the palette constants, color-depth enums, resolved-background
//! structs, and all the low-level styling and color-quantization functions used
//! by both inline rendering and the summary view.

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

use crate::color::is_light;
use crate::color::perceptual_distance;
use crate::render::highlight::DiffScopeBackgroundRgbs;
use crate::render::highlight::diff_scope_background_rgbs;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::XTERM_COLORS;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::indexed_color;
use crate::terminal_palette::rgb_color;
use crate::terminal_palette::stdout_color_level;

/// Display width of a tab character in columns.
pub(super) const TAB_WIDTH: usize = 4;

// -- Diff background palette --------------------------------------------------
//
// Dark-theme tints are subtle enough to avoid clashing with syntax colors.
// Light-theme values match GitHub's diff colors for familiarity.  The gutter
// (line-number column) uses slightly more saturated variants on light
// backgrounds so the numbers remain readable against the pastel line background.
// Truecolor palette.
pub(super) const DARK_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (33, 58, 43); // #213A2B
pub(super) const DARK_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (74, 34, 29); // #4A221D
pub(super) const LIGHT_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (218, 251, 225); // #dafbe1
pub(super) const LIGHT_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (255, 235, 233); // #ffebe9
pub(super) const LIGHT_TC_ADD_NUM_BG_RGB: (u8, u8, u8) = (172, 238, 187); // #aceebb
pub(super) const LIGHT_TC_DEL_NUM_BG_RGB: (u8, u8, u8) = (255, 206, 203); // #ffcecb
pub(super) const LIGHT_TC_GUTTER_FG_RGB: (u8, u8, u8) = (31, 35, 40); // #1f2328

// 256-color palette.
pub(super) const DARK_256_ADD_LINE_BG_IDX: u8 = 22;
pub(super) const DARK_256_DEL_LINE_BG_IDX: u8 = 52;
pub(super) const LIGHT_256_ADD_LINE_BG_IDX: u8 = 194;
pub(super) const LIGHT_256_DEL_LINE_BG_IDX: u8 = 224;
pub(super) const LIGHT_256_ADD_NUM_BG_IDX: u8 = 157;
pub(super) const LIGHT_256_DEL_NUM_BG_IDX: u8 = 217;
pub(super) const LIGHT_256_GUTTER_FG_IDX: u8 = 236;

/// Classifies a diff line for gutter sign rendering and style selection.
///
/// `Insert` renders with a `+` sign and green text, `Delete` with `-` and red
/// text (plus dim overlay when syntax-highlighted), and `Context` with a space
/// and default styling.
#[derive(Clone, Copy)]
pub enum DiffLineType {
    Insert,
    Delete,
    Context,
}

/// Controls which color palette the diff renderer uses for backgrounds and
/// gutter styling.
///
/// Determined once per `render_change` call via [`diff_theme`], which probes
/// the terminal's queried background color.  When the background cannot be
/// determined (common in CI or piped output), `Dark` is used as the safe
/// default.
#[derive(Clone, Copy, Debug)]
pub(super) enum DiffTheme {
    Dark,
    Light,
}

/// Palette depth the diff renderer will target.
///
/// This is the *renderer's own* notion of color depth, derived from the raw
/// [`StdoutColorLevel`] reported by `supports-color` via
/// [`diff_color_level_for_terminal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DiffColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
}

/// Subset of [`DiffColorLevel`] that supports tinted backgrounds.
///
/// ANSI-16 terminals render backgrounds with bold, saturated palette entries
/// that overpower syntax tokens.  This type encodes the invariant "we have
/// enough color depth for pastel tints" so that background-producing helpers
/// (`add_line_bg`, `del_line_bg`, `light_add_num_bg`, `light_del_num_bg`)
/// never need an unreachable ANSI-16 arm.
///
/// Construct via [`RichDiffColorLevel::from_diff_color_level`], which returns
/// `None` for ANSI-16 — callers branch on the `Option` and skip backgrounds
/// entirely when `None`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RichDiffColorLevel {
    TrueColor,
    Ansi256,
}

impl RichDiffColorLevel {
    /// Extract a rich level, returning `None` for ANSI-16.
    pub(super) fn from_diff_color_level(level: DiffColorLevel) -> Option<Self> {
        match level {
            DiffColorLevel::TrueColor => Some(Self::TrueColor),
            DiffColorLevel::Ansi256 => Some(Self::Ansi256),
            DiffColorLevel::Ansi16 => None,
        }
    }
}

/// Pre-resolved background colors for insert and delete diff lines.
///
/// Computed once per `render_change` call from the active syntax theme's
/// scope backgrounds (via [`resolve_diff_backgrounds`]) and then threaded
/// through every style helper so individual lines never re-query the theme.
///
/// Both fields are `None` when the color level is ANSI-16 — callers fall
/// back to foreground-only styling in that case.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ResolvedDiffBackgrounds {
    pub(super) add: Option<Color>,
    pub(super) del: Option<Color>,
}

/// Precomputed render state for diff line styling.
///
/// This bundles the terminal-derived theme and color depth plus theme-resolved
/// diff backgrounds so callers rendering many lines can compute once per render
/// pass and reuse it across all line calls.
#[derive(Clone, Copy, Debug)]
pub struct DiffRenderStyleContext {
    pub(super) theme: DiffTheme,
    pub(super) color_level: DiffColorLevel,
    pub(super) diff_backgrounds: ResolvedDiffBackgrounds,
}

/// Resolve diff backgrounds for production rendering.
///
/// Queries the active syntax theme for `markup.inserted` / `markup.deleted`
/// (and `diff.*` fallbacks), then delegates to [`resolve_diff_backgrounds_for`].
pub(super) fn resolve_diff_backgrounds(
    theme: DiffTheme,
    color_level: DiffColorLevel,
) -> ResolvedDiffBackgrounds {
    resolve_diff_backgrounds_for(theme, color_level, diff_scope_background_rgbs())
}

/// Snapshot the current terminal environment into a reusable style context.
///
/// Queries `diff_theme`, `diff_color_level`, and the active syntax theme's
/// scope backgrounds once, bundling them into a [`DiffRenderStyleContext`]
/// that callers thread through every line-rendering call in a single pass.
///
/// Call this at the top of each render frame — not per line — so the diff
/// palette stays consistent within a frame even if the user swaps themes
/// mid-render (theme picker live preview).
pub fn current_diff_render_style_context() -> DiffRenderStyleContext {
    let theme = diff_theme();
    let color_level = diff_color_level();
    let diff_backgrounds = resolve_diff_backgrounds(theme, color_level);
    DiffRenderStyleContext {
        theme,
        color_level,
        diff_backgrounds,
    }
}

/// Core background-resolution logic, kept pure for testability.
///
/// Starts from the hardcoded fallback palette and then overrides with theme
/// scope backgrounds when both (a) the color level is rich enough and (b) the
/// theme defines a matching scope.  This means the fallback palette is always
/// the baseline and theme scopes are strictly additive.
pub(super) fn resolve_diff_backgrounds_for(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    scope_backgrounds: DiffScopeBackgroundRgbs,
) -> ResolvedDiffBackgrounds {
    let mut resolved = fallback_diff_backgrounds(theme, color_level);
    let Some(level) = RichDiffColorLevel::from_diff_color_level(color_level) else {
        return resolved;
    };

    if let Some(rgb) = scope_backgrounds.inserted {
        resolved.add = Some(color_from_rgb_for_level(rgb, level));
    }
    if let Some(rgb) = scope_backgrounds.deleted {
        resolved.del = Some(color_from_rgb_for_level(rgb, level));
    }
    resolved
}

/// Hardcoded palette backgrounds, used when the syntax theme provides no
/// diff-specific scope backgrounds.  Returns empty backgrounds for ANSI-16.
pub(super) fn fallback_diff_backgrounds(
    theme: DiffTheme,
    color_level: DiffColorLevel,
) -> ResolvedDiffBackgrounds {
    match RichDiffColorLevel::from_diff_color_level(color_level) {
        Some(level) => ResolvedDiffBackgrounds {
            add: Some(add_line_bg(theme, level)),
            del: Some(del_line_bg(theme, level)),
        },
        None => ResolvedDiffBackgrounds::default(),
    }
}

/// Convert an RGB triple to the appropriate ratatui `Color` for the given
/// rich color level — passthrough for truecolor, quantized for ANSI-256.
pub(super) fn color_from_rgb_for_level(
    rgb: (u8, u8, u8),
    color_level: RichDiffColorLevel,
) -> Color {
    match color_level {
        RichDiffColorLevel::TrueColor => rgb_color(rgb),
        RichDiffColorLevel::Ansi256 => quantize_rgb_to_ansi256(rgb),
    }
}

/// Find the closest ANSI-256 color (indices 16–255) to `target` using
/// perceptual distance.
///
/// Skips the first 16 entries (system colors) because their actual RGB
/// values depend on the user's terminal configuration and are unreliable
/// for distance calculations.
pub(super) fn quantize_rgb_to_ansi256(target: (u8, u8, u8)) -> Color {
    let best_index = XTERM_COLORS
        .iter()
        .enumerate()
        .skip(16)
        .min_by(|(_, a), (_, b)| {
            perceptual_distance(**a, target).total_cmp(&perceptual_distance(**b, target))
        })
        .map(|(index, _)| index as u8);
    match best_index {
        Some(index) => indexed_color(index),
        None => indexed_color(DARK_256_ADD_LINE_BG_IDX),
    }
}

pub fn line_number_width(max_line_number: usize) -> usize {
    if max_line_number == 0 {
        1
    } else {
        max_line_number.to_string().len()
    }
}

/// Testable helper: picks `DiffTheme` from an explicit background sample.
pub(super) fn diff_theme_for_bg(bg: Option<(u8, u8, u8)>) -> DiffTheme {
    if let Some(rgb) = bg
        && is_light(rgb)
    {
        return DiffTheme::Light;
    }
    DiffTheme::Dark
}

pub(super) fn diff_theme() -> DiffTheme {
    diff_theme_for_bg(default_bg())
}

pub(super) fn diff_color_level() -> DiffColorLevel {
    diff_color_level_for_terminal(stdout_color_level())
}

pub(super) fn diff_color_level_for_terminal(stdout_level: StdoutColorLevel) -> DiffColorLevel {
    match stdout_level {
        StdoutColorLevel::TrueColor => DiffColorLevel::TrueColor,
        StdoutColorLevel::Ansi256 => DiffColorLevel::Ansi256,
        StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => DiffColorLevel::Ansi16,
    }
}

pub(super) fn style_line_bg_for(
    kind: DiffLineType,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    let bg = match kind {
        DiffLineType::Insert => diff_backgrounds.add,
        DiffLineType::Delete => diff_backgrounds.del,
        DiffLineType::Context => None,
    };
    match bg {
        Some(color) => Style::default().bg(color),
        None => Style::default(),
    }
}

pub(super) fn style_context() -> Style {
    Style::default()
}

pub(super) fn add_line_bg(theme: DiffTheme, color_level: RichDiffColorLevel) -> Color {
    match (theme, color_level) {
        (DiffTheme::Dark, RichDiffColorLevel::TrueColor) => rgb_color(DARK_TC_ADD_LINE_BG_RGB),
        (DiffTheme::Dark, RichDiffColorLevel::Ansi256) => indexed_color(DARK_256_ADD_LINE_BG_IDX),
        (DiffTheme::Light, RichDiffColorLevel::TrueColor) => rgb_color(LIGHT_TC_ADD_LINE_BG_RGB),
        (DiffTheme::Light, RichDiffColorLevel::Ansi256) => indexed_color(LIGHT_256_ADD_LINE_BG_IDX),
    }
}

pub(super) fn del_line_bg(theme: DiffTheme, color_level: RichDiffColorLevel) -> Color {
    match (theme, color_level) {
        (DiffTheme::Dark, RichDiffColorLevel::TrueColor) => rgb_color(DARK_TC_DEL_LINE_BG_RGB),
        (DiffTheme::Dark, RichDiffColorLevel::Ansi256) => indexed_color(DARK_256_DEL_LINE_BG_IDX),
        (DiffTheme::Light, RichDiffColorLevel::TrueColor) => rgb_color(LIGHT_TC_DEL_LINE_BG_RGB),
        (DiffTheme::Light, RichDiffColorLevel::Ansi256) => indexed_color(LIGHT_256_DEL_LINE_BG_IDX),
    }
}

pub(super) fn light_gutter_fg(color_level: DiffColorLevel) -> Color {
    match color_level {
        DiffColorLevel::TrueColor => rgb_color(LIGHT_TC_GUTTER_FG_RGB),
        DiffColorLevel::Ansi256 => indexed_color(LIGHT_256_GUTTER_FG_IDX),
        DiffColorLevel::Ansi16 => Color::Black,
    }
}

pub(super) fn light_add_num_bg(color_level: RichDiffColorLevel) -> Color {
    match color_level {
        RichDiffColorLevel::TrueColor => rgb_color(LIGHT_TC_ADD_NUM_BG_RGB),
        RichDiffColorLevel::Ansi256 => indexed_color(LIGHT_256_ADD_NUM_BG_IDX),
    }
}

pub(super) fn light_del_num_bg(color_level: RichDiffColorLevel) -> Color {
    match color_level {
        RichDiffColorLevel::TrueColor => rgb_color(LIGHT_TC_DEL_NUM_BG_RGB),
        RichDiffColorLevel::Ansi256 => indexed_color(LIGHT_256_DEL_NUM_BG_IDX),
    }
}

pub(super) fn style_gutter_for(
    kind: DiffLineType,
    theme: DiffTheme,
    color_level: DiffColorLevel,
) -> Style {
    match (
        theme,
        kind,
        RichDiffColorLevel::from_diff_color_level(color_level),
    ) {
        (DiffTheme::Light, DiffLineType::Insert, None) => {
            Style::default().fg(light_gutter_fg(color_level))
        }
        (DiffTheme::Light, DiffLineType::Delete, None) => {
            Style::default().fg(light_gutter_fg(color_level))
        }
        (DiffTheme::Light, DiffLineType::Insert, Some(level)) => Style::default()
            .fg(light_gutter_fg(color_level))
            .bg(light_add_num_bg(level)),
        (DiffTheme::Light, DiffLineType::Delete, Some(level)) => Style::default()
            .fg(light_gutter_fg(color_level))
            .bg(light_del_num_bg(level)),
        _ => style_gutter_dim(),
    }
}

/// Sign character (`+`) for insert lines.  On dark terminals it inherits the
/// full content style (green fg + tinted bg).  On light terminals it uses only
/// a green foreground and lets the line-level bg show through.
pub(super) fn style_sign_add(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match theme {
        DiffTheme::Light => Style::default().fg(crate::theme::green()),
        DiffTheme::Dark => style_add(theme, color_level, diff_backgrounds),
    }
}

/// Sign character (`-`) for delete lines.  Mirror of [`style_sign_add`].
pub(super) fn style_sign_del(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match theme {
        DiffTheme::Light => Style::default().fg(crate::theme::red()),
        DiffTheme::Dark => style_del(theme, color_level, diff_backgrounds),
    }
}

/// Content style for insert lines (plain, non-syntax-highlighted text).
pub(super) fn style_add(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match (theme, color_level, diff_backgrounds.add) {
        (_, DiffColorLevel::Ansi16, _) => Style::default().fg(Color::Green),
        (DiffTheme::Light, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Light, DiffColorLevel::Ansi256, Some(bg)) => Style::default().bg(bg),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, Some(bg)) => {
            Style::default().fg(crate::theme::green()).bg(bg)
        }
        (DiffTheme::Light, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Light, DiffColorLevel::Ansi256, None) => Style::default(),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, None) => {
            Style::default().fg(crate::theme::green())
        }
    }
}

/// Content style for delete lines (plain, non-syntax-highlighted text).
pub(super) fn style_del(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match (theme, color_level, diff_backgrounds.del) {
        (_, DiffColorLevel::Ansi16, _) => Style::default().fg(Color::Red),
        (DiffTheme::Light, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Light, DiffColorLevel::Ansi256, Some(bg)) => Style::default().bg(bg),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, Some(bg)) => {
            Style::default().fg(crate::theme::red()).bg(bg)
        }
        (DiffTheme::Light, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Light, DiffColorLevel::Ansi256, None) => Style::default(),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, None) => {
            Style::default().fg(crate::theme::red())
        }
    }
}

pub(super) fn style_gutter_dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
