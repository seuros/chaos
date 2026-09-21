use std::sync::OnceLock;

use ratatui::style::Color;
use termprofile::{DetectorSettings, TermProfile};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdoutColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
    Unknown,
}

pub fn stdout_color_level() -> StdoutColorLevel {
    static PROFILE: OnceLock<TermProfile> = OnceLock::new();
    match PROFILE.get_or_init(|| {
        TermProfile::detect(
            &std::io::stdout(),
            DetectorSettings::new().enable_tmux_info(false),
        )
    }) {
        TermProfile::TrueColor => StdoutColorLevel::TrueColor,
        TermProfile::Ansi256 => StdoutColorLevel::Ansi256,
        TermProfile::Ansi16 => StdoutColorLevel::Ansi16,
        TermProfile::NoColor | TermProfile::NoTty => StdoutColorLevel::Unknown,
    }
}

#[allow(clippy::disallowed_methods)]
pub fn rgb_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

#[allow(clippy::disallowed_methods)]
pub fn indexed_color(index: u8) -> Color {
    Color::Indexed(index)
}

pub fn default_fg() -> Option<(u8, u8, u8)> {
    None
}

pub fn default_bg() -> Option<(u8, u8, u8)> {
    None
}
