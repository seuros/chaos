//! Chaos palette definitions, color constants, and widget styling helpers.
//!
//! Mirrors `bin/console/src/theme.rs`. The TUI uses ratatui's 16-color ANSI
//! palette; here we pick sRGB triples that reproduce the same "green phosphor
//! CRT" feel in a real GPU window, plus an Anthropic-orange variant for the
//! clamped mode toggle. The palette carries *ten* semantic slots so any widget
//! that cares about more than iced's six-slot [`IcedPalette`] (dim, border,
//! accent, highlight) can still pull a color from one place.

use iced::Background;
use iced::Border;
use iced::Color;
use iced::Theme;
use iced::theme::Palette as IcedPalette;
use iced::widget::button;
use iced::widget::container;

/// Semantic color set used throughout `chaos-xclient`.
///
/// Ten slots — same names as `bin/console`'s `Palette`. Kept separate from
/// iced's [`IcedPalette`] because iced only exposes six slots to its theme
/// machinery and we need a few more (dim, border, accent, highlight) to
/// drive our own widget styling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChaosPalette {
    /// Window background.
    pub bg: Color,
    /// Primary text.
    pub fg: Color,
    /// Secondary / muted text.
    pub dim: Color,
    /// Selected / highlighted element.
    pub highlight: Color,
    /// User-message bubble background.
    pub user_msg_bg: Color,
    /// Chrome / borders.
    pub border: Color,
    /// Warning accent.
    pub warning: Color,
    /// Error accent.
    pub error: Color,
    /// Success accent.
    pub success: Color,
    /// Interactive / link accent.
    pub accent: Color,
}

// Named shades mirroring console's ratatui ANSI anchors. We refer to these
// by symbol in both palettes so the equality relations enforced by console's
// theme (dim == border == success in PHOSPHOR, dim == highlight == border ==
// warning == accent in ANTHROPIC) are literally the same `Color` constant —
// not three slightly-different shades that happen to live near each other.
const CHAOS_BLACK: Color = Color::from_rgb(0.0, 0.0, 0.0);
const CHAOS_LIGHT_GREEN: Color = Color::from_rgb(0x55 as f32 / 255.0, 1.0, 0x55 as f32 / 255.0);
const CHAOS_GREEN: Color = Color::from_rgb(0.0, 0xaa as f32 / 255.0, 0.0);
const CHAOS_DARK_GREEN_BG: Color = Color::from_rgb(
    0x08 as f32 / 255.0,
    0x18 as f32 / 255.0,
    0x08 as f32 / 255.0,
);
const CHAOS_LIGHT_CYAN: Color = Color::from_rgb(0x55 as f32 / 255.0, 1.0, 1.0);
const CHAOS_LIGHT_YELLOW: Color = Color::from_rgb(1.0, 1.0, 0x55 as f32 / 255.0);
const CHAOS_LIGHT_RED: Color = Color::from_rgb(1.0, 0x55 as f32 / 255.0, 0x55 as f32 / 255.0);
const CHAOS_WARM_ORANGE: Color = Color::from_rgb(1.0, 0xaa as f32 / 255.0, 0x55 as f32 / 255.0);
const CHAOS_AMBER: Color = Color::from_rgb(
    0xcc as f32 / 255.0,
    0x88 as f32 / 255.0,
    0x33 as f32 / 255.0,
);
const CHAOS_DARK_AMBER_BG: Color = Color::from_rgb(
    0x22 as f32 / 255.0,
    0x18 as f32 / 255.0,
    0x0c as f32 / 255.0,
);

/// Phosphor-green CRT palette — the default.
///
/// Roles track `bin/console`'s `PHOSPHOR` exactly, including the slot
/// equalities that console enforces via a single ratatui `Color` constant:
/// - `fg == highlight` → bright phosphor [`CHAOS_LIGHT_GREEN`]
/// - `dim == border == success` → base green [`CHAOS_GREEN`]
/// - `warning` → amber [`CHAOS_LIGHT_YELLOW`]
/// - `error` → light red [`CHAOS_LIGHT_RED`]
/// - `accent` → cyan [`CHAOS_LIGHT_CYAN`]
pub const PHOSPHOR: ChaosPalette = ChaosPalette {
    bg: CHAOS_BLACK,
    fg: CHAOS_LIGHT_GREEN,
    dim: CHAOS_GREEN,
    highlight: CHAOS_LIGHT_GREEN,
    user_msg_bg: CHAOS_DARK_GREEN_BG,
    border: CHAOS_GREEN,
    warning: CHAOS_LIGHT_YELLOW,
    error: CHAOS_LIGHT_RED,
    success: CHAOS_GREEN,
    accent: CHAOS_LIGHT_CYAN,
};

/// Anthropic-orange palette — used when the GUI is clamped to the
/// Claude Code MAX subscription. Mirrors `bin/console`'s `ANTHROPIC`,
/// where ratatui's single `Color::Yellow` is reused for five slots. We use
/// [`CHAOS_AMBER`] for that collapsed role and [`CHAOS_WARM_ORANGE`] for
/// the brighter `fg == success` pair:
/// - `fg == success` → [`CHAOS_WARM_ORANGE`]
/// - `dim == highlight == border == warning == accent` → [`CHAOS_AMBER`]
/// - `error` → [`CHAOS_LIGHT_RED`] (unchanged from phosphor)
pub const ANTHROPIC: ChaosPalette = ChaosPalette {
    bg: CHAOS_BLACK,
    fg: CHAOS_WARM_ORANGE,
    dim: CHAOS_AMBER,
    highlight: CHAOS_AMBER,
    user_msg_bg: CHAOS_DARK_AMBER_BG,
    border: CHAOS_AMBER,
    warning: CHAOS_AMBER,
    error: CHAOS_LIGHT_RED,
    success: CHAOS_WARM_ORANGE,
    accent: CHAOS_AMBER,
};

impl ChaosPalette {
    /// Project the ten-slot chaos palette onto iced's six-slot palette.
    ///
    /// This is what iced's theme machinery actually reads — built-in widgets
    /// (pick_list, scrollbar, etc.) will query the resulting `IcedPalette`
    /// through the extended palette generator. The chaos-specific slots
    /// (dim/border/accent/highlight/user_msg_bg) stay accessible via the
    /// methods on `ChaosWindow` for widgets we style by hand.
    pub const fn to_iced_palette(self) -> IcedPalette {
        IcedPalette {
            background: self.bg,
            text: self.fg,
            primary: self.highlight,
            success: self.success,
            warning: self.warning,
            danger: self.error,
        }
    }

    /// Build an iced [`Theme::Custom`] from this palette. Name is stable
    /// per-palette so iced can cache and diff them correctly.
    pub fn to_theme(self, name: &'static str) -> Theme {
        Theme::custom(name.to_string(), self.to_iced_palette())
    }
}

/// `container::Style` closure that paints the chaos background + text color.
/// Used at the root so the window isn't iced's default dark gray.
pub(super) fn container_root(palette: ChaosPalette) -> container::Style {
    container::Style {
        background: Some(Background::Color(palette.bg)),
        text_color: Some(palette.fg),
        ..container::Style::default()
    }
}

/// `container::Style` closure for the transcript: background + muted border
/// so the scrollable region is visually distinct from the composer.
pub(super) fn container_transcript(palette: ChaosPalette) -> container::Style {
    container::Style {
        background: Some(Background::Color(palette.bg)),
        text_color: Some(palette.fg),
        border: Border {
            color: palette.border,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..container::Style::default()
    }
}

/// `container::Style` closure for a user-submitted message bubble.
pub(super) fn container_user(palette: ChaosPalette) -> container::Style {
    container::Style {
        background: Some(Background::Color(palette.user_msg_bg)),
        text_color: Some(palette.fg),
        border: Border {
            color: palette.accent,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..container::Style::default()
    }
}

/// `container::Style` closure for exec/tool blocks — faint border, no fill.
pub(super) fn container_code(palette: ChaosPalette) -> container::Style {
    container::Style {
        background: None,
        text_color: Some(palette.success),
        border: Border {
            color: palette.dim,
            width: 1.0,
            radius: 2.0.into(),
        },
        ..container::Style::default()
    }
}

/// Chaos-flavored primary button style.
///
/// * `Active` — highlight color on background.
/// * `Hovered` — brighten to fg color so the hover state is obvious.
/// * `Pressed` — swap to the accent (cyan in phosphor) so the press
///   gesture is visibly distinct from the resting state; without this the
///   button gave no pressed feedback at all.
/// * `Disabled` — border color on dim so the widget still reads.
pub(super) fn button_primary(
    palette: ChaosPalette,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, fg) = match status {
            button::Status::Active => (palette.highlight, palette.bg),
            button::Status::Hovered => (palette.fg, palette.bg),
            button::Status::Pressed => (palette.accent, palette.bg),
            button::Status::Disabled => (palette.border, palette.dim),
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: fg,
            border: Border {
                color: palette.border,
                width: 1.0,
                radius: 2.0.into(),
            },
            ..button::Style::default()
        }
    }
}

/// Secondary/ghost button for low-emphasis actions (Interrupt, Theme toggle).
pub(super) fn button_ghost(
    palette: ChaosPalette,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let fg = match status {
            button::Status::Disabled => palette.dim,
            button::Status::Hovered => palette.highlight,
            _ => palette.fg,
        };
        button::Style {
            background: None,
            text_color: fg,
            border: Border {
                color: palette.border,
                width: 1.0,
                radius: 2.0.into(),
            },
            ..button::Style::default()
        }
    }
}
