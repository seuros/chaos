use rama::telemetry::tracing;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::RwLock;
use syntect::highlighting::{Highlighter, Theme, ThemeSet};
use syntect::parsing::Scope;
use two_face::theme::EmbeddedThemeName;

use super::singletons::{CUSTOM_THEMES, LIGHT_BG, THEME, THEME_OVERRIDE};
use crate::DiffScopeBackgroundRgbs;

/// All 32 bundled theme names in kebab-case, ordered alphabetically.
pub(super) const BUILTIN_THEME_NAMES: &[&str] = &[
    "1337",
    "ansi",
    "base16",
    "base16-256",
    "base16-eighties-dark",
    "base16-mocha-dark",
    "base16-ocean-dark",
    "base16-ocean-light",
    "catppuccin-frappe",
    "catppuccin-latte",
    "catppuccin-macchiato",
    "catppuccin-mocha",
    "coldark-cold",
    "coldark-dark",
    "dark-neon",
    "dracula",
    "github",
    "gruvbox-dark",
    "gruvbox-light",
    "inspired-github",
    "monokai-extended",
    "monokai-extended-bright",
    "monokai-extended-light",
    "monokai-extended-origin",
    "nord",
    "one-half-dark",
    "one-half-light",
    "solarized-dark",
    "solarized-light",
    "sublime-snazzy",
    "two-dark",
    "zenburn",
];

/// A theme available in the picker, either bundled or loaded from a custom
/// `.tmTheme` file under `{CHAOS_HOME}/themes/`.
pub struct ThemeEntry {
    /// Kebab-case identifier used for config persistence and theme resolution.
    pub name: String,
    /// `true` when this entry was discovered from a `.tmTheme` file on disk
    /// rather than the embedded two-face bundle.
    pub is_custom: bool,
}

/// Map a kebab-case theme name to the corresponding `EmbeddedThemeName`.
pub(super) fn parse_theme_name(name: &str) -> Option<EmbeddedThemeName> {
    match name {
        "ansi" => Some(EmbeddedThemeName::Ansi),
        "base16" => Some(EmbeddedThemeName::Base16),
        "base16-eighties-dark" => Some(EmbeddedThemeName::Base16EightiesDark),
        "base16-mocha-dark" => Some(EmbeddedThemeName::Base16MochaDark),
        "base16-ocean-dark" => Some(EmbeddedThemeName::Base16OceanDark),
        "base16-ocean-light" => Some(EmbeddedThemeName::Base16OceanLight),
        "base16-256" => Some(EmbeddedThemeName::Base16_256),
        "catppuccin-frappe" => Some(EmbeddedThemeName::CatppuccinFrappe),
        "catppuccin-latte" => Some(EmbeddedThemeName::CatppuccinLatte),
        "catppuccin-macchiato" => Some(EmbeddedThemeName::CatppuccinMacchiato),
        "catppuccin-mocha" => Some(EmbeddedThemeName::CatppuccinMocha),
        "coldark-cold" => Some(EmbeddedThemeName::ColdarkCold),
        "coldark-dark" => Some(EmbeddedThemeName::ColdarkDark),
        "dark-neon" => Some(EmbeddedThemeName::DarkNeon),
        "dracula" => Some(EmbeddedThemeName::Dracula),
        "github" => Some(EmbeddedThemeName::Github),
        "gruvbox-dark" => Some(EmbeddedThemeName::GruvboxDark),
        "gruvbox-light" => Some(EmbeddedThemeName::GruvboxLight),
        "inspired-github" => Some(EmbeddedThemeName::InspiredGithub),
        "1337" => Some(EmbeddedThemeName::Leet),
        "monokai-extended" => Some(EmbeddedThemeName::MonokaiExtended),
        "monokai-extended-bright" => Some(EmbeddedThemeName::MonokaiExtendedBright),
        "monokai-extended-light" => Some(EmbeddedThemeName::MonokaiExtendedLight),
        "monokai-extended-origin" => Some(EmbeddedThemeName::MonokaiExtendedOrigin),
        "nord" => Some(EmbeddedThemeName::Nord),
        "one-half-dark" => Some(EmbeddedThemeName::OneHalfDark),
        "one-half-light" => Some(EmbeddedThemeName::OneHalfLight),
        "solarized-dark" => Some(EmbeddedThemeName::SolarizedDark),
        "solarized-light" => Some(EmbeddedThemeName::SolarizedLight),
        "sublime-snazzy" => Some(EmbeddedThemeName::SublimeSnazzy),
        "two-dark" => Some(EmbeddedThemeName::TwoDark),
        "zenburn" => Some(EmbeddedThemeName::Zenburn),
        _ => None,
    }
}

/// Parse a `.tmTheme` property list from memory.
pub fn parse_tmtheme(source: &str) -> Option<Theme> {
    ThemeSet::load_from_reader(&mut Cursor::new(source.as_bytes())).ok()
}

/// Install the custom themes for this process (write-once, like the theme
/// override). Returns `false` when they were already installed.
pub fn set_custom_themes(themes: BTreeMap<String, Theme>) -> bool {
    let installed = CUSTOM_THEMES.set(themes).is_ok();
    if installed && THEME.get().is_some() {
        set_syntax_theme(build_default_theme());
    }
    installed
}

/// A custom theme by name. Built-ins shadow custom themes with the same name.
fn custom_theme(name: &str) -> Option<Theme> {
    CUSTOM_THEMES.get()?.get(name).cloned()
}

pub(super) fn adaptive_default_theme_selection() -> (EmbeddedThemeName, &'static str) {
    let is_light = LIGHT_BG.get().copied().flatten().unwrap_or(false);
    if is_light {
        (EmbeddedThemeName::CatppuccinLatte, "catppuccin-latte")
    } else {
        (EmbeddedThemeName::CatppuccinMocha, "catppuccin-mocha")
    }
}

pub(super) fn adaptive_default_embedded_theme_name() -> EmbeddedThemeName {
    adaptive_default_theme_selection().0
}

/// Return the kebab-case name of the adaptive default syntax theme selected
/// from terminal background lightness.
pub fn adaptive_default_theme_name() -> &'static str {
    adaptive_default_theme_selection().1
}

/// Build the theme from current override/default-theme settings.
pub(super) fn resolve_theme_with_override(name: Option<&str>) -> Theme {
    let ts = two_face::theme::extra();

    if let Some(name) = name {
        if let Some(theme_name) = parse_theme_name(name) {
            return ts.get(theme_name).clone();
        }
        if let Some(theme) = custom_theme(name) {
            return theme;
        }
        tracing::debug!("Theme \"{name}\" not recognized; using default theme");
    }

    ts.get(adaptive_default_embedded_theme_name()).clone()
}

/// Build the theme from current override/default-theme settings.
pub(super) fn build_default_theme() -> Theme {
    let name = THEME_OVERRIDE.get().and_then(|name| name.as_deref());
    resolve_theme_with_override(name)
}

pub(super) fn theme_lock() -> &'static RwLock<Theme> {
    THEME.get_or_init(|| RwLock::new(build_default_theme()))
}

/// Swap the active syntax theme at runtime (for live preview).
pub fn set_syntax_theme(theme: Theme) {
    let mut guard = match theme_lock().write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = theme;
}

/// Clone the current syntax theme (e.g. to save for cancel-restore).
pub fn current_syntax_theme() -> Theme {
    match theme_lock().read() {
        Ok(theme) => theme.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Set the user-configured syntax theme override, chaos home path, and
/// terminal background lightness hint.
///
/// Call this with the **final resolved config** (after onboarding, resume, and
/// fork reloads complete). The first call persists `name`, `chaos_home`, and
/// `is_light_background` in `OnceLock`s used by startup/default theme
/// resolution.
///
/// Subsequent calls cannot change the persisted `OnceLock` values, but they
/// still update the runtime theme immediately for live preview flows.
///
/// Returns user-facing warnings for actionable configuration issues, such as
/// unknown/invalid theme names or duplicate override persistence.
pub fn set_theme_override(
    name: Option<String>,
    is_light_background: Option<bool>,
) -> Option<String> {
    let warning = validate_theme_name(name.as_deref());
    let override_set_ok = THEME_OVERRIDE.set(name.clone()).is_ok();
    let _ = LIGHT_BG.set(is_light_background);
    if THEME.get().is_some() {
        set_syntax_theme(resolve_theme_with_override(name.as_deref()));
    }
    if !override_set_ok {
        tracing::debug!("set_theme_override called more than once; OnceLock values unchanged");
    }
    warning
}

/// Check whether a theme name resolves to a bundled or stored custom theme.
/// Returns a user-facing warning when it does not.
pub fn validate_theme_name(name: Option<&str>) -> Option<String> {
    let name = name?;
    if parse_theme_name(name).is_some() || custom_theme(name).is_some() {
        return None;
    }
    Some(format!(
        "Theme \"{name}\" not found. Using the default theme. \
         Import a custom theme with `chaos themes import <file.tmTheme>`."
    ))
}

/// Return the configured kebab-case theme name when it resolves; otherwise
/// return the adaptive auto-detected default theme name.
///
/// This intentionally reflects persisted configuration/default selection, not
/// transient runtime swaps applied via `set_syntax_theme`.
pub fn configured_theme_name() -> String {
    if let Some(Some(name)) = THEME_OVERRIDE.get() {
        if parse_theme_name(name).is_some() {
            return name.clone();
        }
        if custom_theme(name).is_some() {
            return name.clone();
        }
    }
    adaptive_default_theme_name().to_string()
}

/// Resolve a theme name to a `Theme` (bundled or stored custom). Returns
/// `None` when the name is unknown.
pub fn resolve_theme_by_name(name: &str) -> Option<Theme> {
    let ts = two_face::theme::extra();
    if let Some(embedded) = parse_theme_name(name) {
        return Some(ts.get(embedded).clone());
    }
    custom_theme(name)
}

/// Query the active syntax theme for diff-scope background colors.
pub fn diff_scope_background_rgbs_for_theme(theme: &Theme) -> DiffScopeBackgroundRgbs {
    let highlighter = Highlighter::new(theme);
    let inserted = scope_background_rgb(&highlighter, "markup.inserted")
        .or_else(|| scope_background_rgb(&highlighter, "diff.inserted"));
    let deleted = scope_background_rgb(&highlighter, "markup.deleted")
        .or_else(|| scope_background_rgb(&highlighter, "diff.deleted"));
    DiffScopeBackgroundRgbs { inserted, deleted }
}

/// Extract the background color for a single TextMate scope, if defined.
fn scope_background_rgb(highlighter: &Highlighter<'_>, scope_name: &str) -> Option<(u8, u8, u8)> {
    let scope = Scope::new(scope_name).ok()?;
    let bg = highlighter.style_mod_for_stack(&[scope]).background?;
    Some((bg.r, bg.g, bg.b))
}

/// List all available theme names: bundled themes plus stored custom themes.
pub fn list_available_themes() -> Vec<ThemeEntry> {
    let mut entries: Vec<ThemeEntry> = BUILTIN_THEME_NAMES
        .iter()
        .map(|name| ThemeEntry {
            name: name.to_string(),
            is_custom: false,
        })
        .collect();

    if let Some(custom) = CUSTOM_THEMES.get() {
        for name in custom.keys() {
            if !entries.iter().any(|entry| &entry.name == name) {
                entries.push(ThemeEntry {
                    name: name.clone(),
                    is_custom: true,
                });
            }
        }
    }

    entries.sort_by_cached_key(|entry| (entry.name.to_ascii_lowercase(), entry.name.clone()));

    entries
}
