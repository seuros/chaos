use crate::theme;
use ratatui::style::Style;

pub fn user_message_style() -> Style {
    theme::user_message()
}

pub fn proposed_plan_style() -> Style {
    theme::user_message()
}
