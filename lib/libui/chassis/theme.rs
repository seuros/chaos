//! Shared frontend theme tokens.
//!
//! `chaos-chassis` owns the semantic palette layout; renderer-specific crates
//! map these tone tokens to concrete ratatui / iced colors.

/// High-level theme family used across frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeFamily {
    Phosphor,
    Anthropic,
}

/// Abstract tone token. Renderers decide how to materialize these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToneToken {
    Black,
    LightGreen,
    Green,
    DarkGray,
    Yellow,
    LightRed,
    Cyan,
    WarmOrange,
    Amber,
    DarkGreenBg,
    DarkAmberBg,
}

/// Semantic palette slots shared by every renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Palette<T = ToneToken> {
    pub bg: T,
    pub fg: T,
    pub dim: T,
    pub highlight: T,
    pub user_msg_bg: T,
    pub border: T,
    pub warning: T,
    pub error: T,
    pub success: T,
    pub accent: T,
}

impl<T: Copy> Palette<T> {
    pub fn map<U>(self, map: impl Fn(T) -> U) -> Palette<U> {
        Palette {
            bg: map(self.bg),
            fg: map(self.fg),
            dim: map(self.dim),
            highlight: map(self.highlight),
            user_msg_bg: map(self.user_msg_bg),
            border: map(self.border),
            warning: map(self.warning),
            error: map(self.error),
            success: map(self.success),
            accent: map(self.accent),
        }
    }
}

impl ThemeFamily {
    pub const fn tokens(self) -> Palette<ToneToken> {
        match self {
            Self::Phosphor => Palette {
                bg: ToneToken::Black,
                fg: ToneToken::LightGreen,
                dim: ToneToken::Green,
                highlight: ToneToken::LightGreen,
                user_msg_bg: ToneToken::DarkGray,
                border: ToneToken::Green,
                warning: ToneToken::Yellow,
                error: ToneToken::LightRed,
                success: ToneToken::Green,
                accent: ToneToken::Cyan,
            },
            Self::Anthropic => Palette {
                bg: ToneToken::Black,
                fg: ToneToken::WarmOrange,
                dim: ToneToken::Amber,
                highlight: ToneToken::Amber,
                user_msg_bg: ToneToken::DarkGray,
                border: ToneToken::Amber,
                warning: ToneToken::Amber,
                error: ToneToken::LightRed,
                success: ToneToken::WarmOrange,
                accent: ToneToken::Amber,
            },
        }
    }
}
