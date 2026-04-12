//! Re-export shim for the `chaos-highlight` crate.
//!
//! When the `syntax` feature is enabled, most public items from
//! `chaos_highlight` are re-exported so existing `crate::render::highlight::`
//! paths continue to resolve.  `set_theme_override` is wrapped to inject
//! terminal background lightness from console's own palette detection.
//!
//! When the feature is disabled, stub functions return empty/None fallbacks
//! so callers degrade to plain unstyled text.

// -- Feature-gated re-exports -------------------------------------------------

#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::DiffScopeBackgroundRgbs;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::ThemeEntry;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::adaptive_default_theme_name;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::configured_theme_name;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::current_syntax_theme;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::diff_scope_background_rgbs;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::exceeds_highlight_limits;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::highlight_bash_to_lines;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::highlight_code_to_lines;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::highlight_code_to_styled_spans;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::list_available_themes;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::resolve_theme_by_name;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::set_syntax_theme;
#[cfg(feature = "syntax")]
#[allow(unused_imports)]
pub use chaos_highlight::validate_theme_name;

/// Wrapper around [`chaos_highlight::set_theme_override`] that injects
/// terminal background lightness from console's palette detection, preserving
/// the original two-argument call signature used throughout the codebase.
#[cfg(feature = "syntax")]
pub fn set_theme_override(
    name: Option<String>,
    chaos_home: Option<std::path::PathBuf>,
) -> Option<String> {
    let is_light = crate::terminal_palette::default_bg().map(crate::color::is_light);
    chaos_highlight::set_theme_override(name, chaos_home, is_light)
}

// -- Stubs when the `syntax` feature is disabled ------------------------------

#[cfg(not(feature = "syntax"))]
pub use self::stubs::*;

#[cfg(not(feature = "syntax"))]
mod stubs {
    use ratatui::text::Line;
    use ratatui::text::Span;
    use std::path::Path;
    use std::path::PathBuf;

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct DiffScopeBackgroundRgbs {
        pub inserted: Option<(u8, u8, u8)>,
        pub deleted: Option<(u8, u8, u8)>,
    }

    pub struct ThemeEntry {
        pub name: String,
        pub is_custom: bool,
    }

    pub fn set_theme_override(
        _name: Option<String>,
        _codex_home: Option<PathBuf>,
    ) -> Option<String> {
        None
    }

    pub fn validate_theme_name(_name: Option<&str>, _codex_home: Option<&Path>) -> Option<String> {
        None
    }

    pub fn adaptive_default_theme_name() -> &'static str {
        "catppuccin-mocha"
    }

    pub fn configured_theme_name() -> String {
        adaptive_default_theme_name().to_string()
    }

    /// Opaque theme placeholder when syntax highlighting is disabled.
    #[derive(Clone, Debug)]
    pub struct Theme;

    pub fn set_syntax_theme(_theme: Theme) {}

    pub fn current_syntax_theme() -> Theme {
        Theme
    }

    pub fn resolve_theme_by_name(_name: &str, _codex_home: Option<&Path>) -> Option<Theme> {
        None
    }

    pub fn list_available_themes(_codex_home: Option<&Path>) -> Vec<ThemeEntry> {
        Vec::new()
    }

    pub fn exceeds_highlight_limits(_total_bytes: usize, _total_lines: usize) -> bool {
        true
    }

    pub fn highlight_code_to_lines(code: &str, _lang: &str) -> Vec<Line<'static>> {
        let mut result: Vec<Line<'static>> =
            code.lines().map(|l| Line::from(l.to_string())).collect();
        if result.is_empty() {
            result.push(Line::from(String::new()));
        }
        result
    }

    pub fn highlight_bash_to_lines(script: &str) -> Vec<Line<'static>> {
        highlight_code_to_lines(script, "bash")
    }

    pub fn highlight_code_to_styled_spans(
        _code: &str,
        _lang: &str,
    ) -> Option<Vec<Vec<Span<'static>>>> {
        None
    }

    pub fn diff_scope_background_rgbs() -> DiffScopeBackgroundRgbs {
        DiffScopeBackgroundRgbs::default()
    }
}
