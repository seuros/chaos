use crate::theme;
use ratatui::style::Style;

pub fn user_message_style() -> Style {
    theme::user_message()
}

pub fn proposed_plan_style() -> Style {
    theme::user_message()
}

/// Returns the style for a user-authored message using the provided terminal background.
/// With the Fallout theme active, we ignore the terminal bg and use the palette.
pub fn user_message_style_for(_terminal_bg: Option<(u8, u8, u8)>) -> Style {
    theme::user_message()
}

pub fn proposed_plan_style_for(_terminal_bg: Option<(u8, u8, u8)>) -> Style {
    theme::user_message()
}

#[allow(clippy::disallowed_methods)]
pub fn user_message_bg(_terminal_bg: (u8, u8, u8)) -> ratatui::style::Color {
    theme::palette().user_msg_bg
}

#[allow(clippy::disallowed_methods)]
pub fn proposed_plan_bg(_terminal_bg: (u8, u8, u8)) -> ratatui::style::Color {
    theme::palette().user_msg_bg
}
