#![warn(rust_2024_compatibility, clippy::all)]

//! Syntax highlighting engine for the Chaos TUI.
//!
//! Wraps [syntect] with the [two_face] grammar and theme bundles to provide
//! ~250-language syntax highlighting and 32 bundled color themes.  The crate
//! owns five process-global singletons:
//!
//! | Singleton | Type | Purpose |
//! |---|---|---|
//! | `SYNTAX_SET` | `OnceLock<SyntaxSet>` | Grammar database, immutable after init |
//! | `THEME` | `OnceLock<RwLock<Theme>>` | Active color theme, swappable at runtime |
//! | `THEME_OVERRIDE` | `OnceLock<Option<String>>` | Persisted user preference (write-once) |
//! | `CHAOS_HOME` | `OnceLock<Option<PathBuf>>` | Root for custom `.tmTheme` discovery |
//! | `LIGHT_BG` | `OnceLock<Option<bool>>` | Terminal background lightness hint |
//!
//! **Lifecycle:** call [`set_theme_override`] once at startup (after the final
//! config is resolved) to persist the user preference and seed the `THEME`
//! lock.  After that, [`set_syntax_theme`] and [`current_syntax_theme`] can
//! swap/snapshot the theme for live preview.  All highlighting functions read
//! the theme via `theme_lock()`.
//!
//! **Guardrails:** inputs exceeding 512 KB or 10 000 lines are rejected early
//! (returns `None`) to prevent pathological CPU/memory usage.  Callers must
//! fall back to plain unstyled text.

mod highlight_engine;
mod singletons;
mod style_conversion;
mod syntax_lookup;
mod theme_management;

pub use highlight_engine::{
    exceeds_highlight_limits, highlight_bash_to_lines, highlight_code_to_lines,
    highlight_code_to_styled_spans,
};
pub use theme_management::{
    ThemeEntry, adaptive_default_theme_name, configured_theme_name, current_syntax_theme,
    diff_scope_background_rgbs_for_theme, list_available_themes, resolve_theme_by_name,
    set_syntax_theme, set_theme_override, validate_theme_name,
};

// NOTE: We intentionally do NOT emit a runtime diagnostic when an ANSI-family
// theme (ansi, base16, base16-256) lacks the expected alpha-channel marker
// encoding.  If the upstream two_face/syntect theme format changes, the
// `ansi_themes_use_only_ansi_palette_colors` test will catch it at build
// time -- long before it reaches users.  A runtime warning would be
// unactionable noise since users can't fix upstream themes.

/// Raw RGB background colors extracted from syntax theme diff/markup scopes.
///
/// These are theme-provided colors, not yet adapted for any particular color
/// depth.  The diff renderer converts them to ratatui `Color` values via
/// `color_from_rgb_for_level` after deciding whether to emit truecolor or
/// quantized ANSI-256.
///
/// Both fields are `None` when the active theme defines no relevant scope
/// backgrounds, in which case the diff renderer falls back to its hardcoded
/// palette.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiffScopeBackgroundRgbs {
    pub inserted: Option<(u8, u8, u8)>,
    pub deleted: Option<(u8, u8, u8)>,
}

/// Query the active syntax theme for diff-scope background colors.
///
/// Prefers `markup.inserted` / `markup.deleted` (the TextMate convention used
/// by most VS Code themes) and falls back to `diff.inserted` / `diff.deleted`
/// (used by some older `.tmTheme` files).
pub fn diff_scope_background_rgbs() -> DiffScopeBackgroundRgbs {
    let theme = current_syntax_theme();
    diff_scope_background_rgbs_for_theme(&theme)
}

#[cfg(test)]
mod tests;
