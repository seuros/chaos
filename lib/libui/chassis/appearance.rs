//! Optional, renderer-independent terminal appearance overrides.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A named ANSI color, `default`, or a six-digit `#RRGGBB` color.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct ThemeColor(String);

impl ThemeColor {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ThemeColor {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let named = matches!(
            value.as_str(),
            "default"
                | "black"
                | "red"
                | "green"
                | "yellow"
                | "blue"
                | "magenta"
                | "cyan"
                | "gray"
                | "dark_gray"
                | "light_red"
                | "light_green"
                | "light_yellow"
                | "light_blue"
                | "light_magenta"
                | "light_cyan"
                | "white"
        );
        let hex = value.len() == 7
            && value.starts_with('#')
            && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit);
        if named || hex {
            Ok(Self(value))
        } else {
            Err(format!(
                "invalid appearance color {value:?}: expected a named ANSI color (such as blue or light_blue), default, or #RRGGBB"
            ))
        }
    }
}

impl From<ThemeColor> for String {
    fn from(value: ThemeColor) -> Self {
        value.0
    }
}

/// Base text styling; Markdown emphasis is applied on top of these settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TextStyle {
    pub fg: Option<ThemeColor>,
    pub bg: Option<ThemeColor>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
}

/// Overrides for the existing semantic palette slots.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PaletteOverrides {
    pub bg: Option<ThemeColor>,
    pub fg: Option<ThemeColor>,
    pub dim: Option<ThemeColor>,
    pub highlight: Option<ThemeColor>,
    pub top_bar_bg: Option<ThemeColor>,
    pub top_bar_fg: Option<ThemeColor>,
    pub top_bar_dim: Option<ThemeColor>,
    pub user_msg_bg: Option<ThemeColor>,
    pub border: Option<ThemeColor>,
    pub warning: Option<ThemeColor>,
    pub error: Option<ThemeColor>,
    pub success: Option<ThemeColor>,
    pub accent: Option<ThemeColor>,
    pub secondary_accent: Option<ThemeColor>,
    pub tertiary_accent: Option<ThemeColor>,
}

/// User configuration layered over the frontend's existing appearance.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub colors: PaletteOverrides,
    pub user: TextStyle,
    pub assistant: TextStyle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_overrides_and_round_trip() {
        let appearance: Appearance = toml::from_str(
            "[colors]\nfg = '#aAbBcC'\nbg = 'default'\n[user]\nbold = true\n[assistant]\nitalic = false",
        ).unwrap();
        assert_eq!(appearance.colors.fg.as_ref().unwrap().as_str(), "#aAbBcC");
        assert_eq!(appearance.user.bold, Some(true));
        assert_eq!(appearance.assistant.italic, Some(false));
        assert_eq!(appearance.user.italic, None);
        assert_eq!(
            appearance,
            toml::from_str(&toml::to_string(&appearance).unwrap()).unwrap()
        );
        assert_eq!(
            toml::from_str::<Appearance>("").unwrap(),
            Appearance::default()
        );
    }

    #[test]
    fn rejects_invalid_colors_and_unknown_fields() {
        // Renderer aliases must not widen the public configuration vocabulary.
        for value in [
            "#fff",
            "#gg0000",
            "orange",
            "\u{1b}[31m",
            "#ééé",
            "Blue",
            "bright_blue",
            "light-blue",
            "reset",
            "12",
            " blue ",
        ] {
            assert!(ThemeColor::try_from(value.to_owned()).is_err(), "{value}");
        }
        for source in [
            "[colors]\nfg = 123",
            "[colors]\nforeground = 'blue'",
            "[user]\nfont = 'Mono'",
            "[assistant]\nbold = 'yes'",
            "unknown = true",
        ] {
            assert!(toml::from_str::<Appearance>(source).is_err(), "{source}");
        }
        for value in ["blue", "light_blue", "default", "#00FF00"] {
            assert!(ThemeColor::try_from(value.to_owned()).is_ok());
        }
    }
}
