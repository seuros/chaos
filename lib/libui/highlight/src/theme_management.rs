use std::path::{Path, PathBuf};
use std::sync::RwLock;
use syntect::highlighting::{Highlighter, Theme, ThemeSet};
use syntect::parsing::Scope;
use two_face::theme::EmbeddedThemeName;

use super::singletons::{CHAOS_HOME, LIGHT_BG, THEME, THEME_OVERRIDE};
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

/// Build the expected path for a custom theme file.
pub(super) fn custom_theme_path(name: &str, chaos_home: &Path) -> PathBuf {
    chaos_home.join("themes").join(format!("{name}.tmTheme"))
}

/// Try to load a custom `.tmTheme` file from `{chaos_home}/themes/{name}.tmTheme`.
pub(super) fn load_custom_theme(name: &str, chaos_home: &Path) -> Option<Theme> {
    ThemeSet::get_theme(custom_theme_path(name, chaos_home)).ok()
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
pub(super) fn resolve_theme_with_override(name: Option<&str>, chaos_home: Option<&Path>) -> Theme {
    let ts = two_face::theme::extra();

    if let Some(name) = name {
        if let Some(theme_name) = parse_theme_name(name) {
            return ts.get(theme_name).clone();
        }
        if let Some(home) = chaos_home
            && let Some(theme) = load_custom_theme(name, home)
        {
            return theme;
        }
        tracing::debug!("Theme \"{name}\" not recognized; using default theme");
    }

    ts.get(adaptive_default_embedded_theme_name()).clone()
}

/// Build the theme from current override/default-theme settings.
pub(super) fn build_default_theme() -> Theme {
    let name = THEME_OVERRIDE.get().and_then(|name| name.as_deref());
    let chaos_home = CHAOS_HOME
        .get()
        .and_then(|chaos_home| chaos_home.as_deref());
    resolve_theme_with_override(name, chaos_home)
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
    chaos_home: Option<PathBuf>,
    is_light_background: Option<bool>,
) -> Option<String> {
    let warning = validate_theme_name(name.as_deref(), chaos_home.as_deref());
    let override_set_ok = THEME_OVERRIDE.set(name.clone()).is_ok();
    let chaos_home_set_ok = CHAOS_HOME.set(chaos_home.clone()).is_ok();
    let _ = LIGHT_BG.set(is_light_background);
    if THEME.get().is_some() {
        set_syntax_theme(resolve_theme_with_override(
            name.as_deref(),
            chaos_home.as_deref(),
        ));
    }
    if !override_set_ok || !chaos_home_set_ok {
        tracing::debug!("set_theme_override called more than once; OnceLock values unchanged");
    }
    warning
}

/// Check whether a theme name resolves to a bundled theme or a custom
/// `.tmTheme` file.  Returns a user-facing warning when it does not.
pub fn validate_theme_name(name: Option<&str>, chaos_home: Option<&Path>) -> Option<String> {
    let name = name?;
    let custom_theme_path_display = chaos_home
        .map(|home| custom_theme_path(name, home).display().to_string())
        .unwrap_or_else(|| format!("$CHAOS_HOME/themes/{name}.tmTheme"));
    if parse_theme_name(name).is_some() {
        return None;
    }
    if let Some(home) = chaos_home {
        let custom_path = custom_theme_path(name, home);
        if custom_path.is_file() {
            if load_custom_theme(name, home).is_some() {
                return None;
            }
            return Some(format!(
                "Custom theme \"{name}\" at {custom_theme_path_display} could not \
                 be loaded (invalid .tmTheme format). Falling back to the default theme."
            ));
        }
    }
    Some(format!(
        "Theme \"{name}\" not found. Using the default theme. \
         To use a custom theme, place a .tmTheme file at \
         {custom_theme_path_display}."
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
        if let Some(Some(home)) = CHAOS_HOME.get()
            && load_custom_theme(name, home).is_some()
        {
            return name.clone();
        }
    }
    adaptive_default_theme_name().to_string()
}

/// Resolve a theme name to a `Theme` (bundled or custom). Returns `None`
/// when the name is unknown and no matching `.tmTheme` file exists.
pub fn resolve_theme_by_name(name: &str, chaos_home: Option<&Path>) -> Option<Theme> {
    let ts = two_face::theme::extra();
    if let Some(embedded) = parse_theme_name(name) {
        return Some(ts.get(embedded).clone());
    }
    if let Some(home) = chaos_home
        && let Some(theme) = load_custom_theme(name, home)
    {
        return Some(theme);
    }
    None
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

/// List all available theme names: bundled themes + custom `.tmTheme` files
/// found in `{chaos_home}/themes/`.
pub fn list_available_themes(chaos_home: Option<&Path>) -> Vec<ThemeEntry> {
    let mut entries: Vec<ThemeEntry> = BUILTIN_THEME_NAMES
        .iter()
        .map(|name| ThemeEntry {
            name: name.to_string(),
            is_custom: false,
        })
        .collect();

    if let Some(home) = chaos_home {
        let themes_dir = home.join("themes");
        if let Ok(read_dir) = std::fs::read_dir(&themes_dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("tmTheme")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    let name = stem.to_string();
                    let is_valid_theme = ThemeSet::get_theme(&path).is_ok();
                    if is_valid_theme && !entries.iter().any(|e| e.name == name) {
                        entries.push(ThemeEntry {
                            name,
                            is_custom: true,
                        });
                    }
                }
            }
        }
    }

    entries.sort_by_cached_key(|entry| (entry.name.to_ascii_lowercase(), entry.name.clone()));

    entries
}
