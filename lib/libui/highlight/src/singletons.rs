use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::RwLock;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

pub(super) static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
pub(super) static THEME: OnceLock<RwLock<Theme>> = OnceLock::new();
pub(super) static THEME_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();
/// Custom themes loaded from the runtime database at startup, by name.
pub(super) static CUSTOM_THEMES: OnceLock<BTreeMap<String, Theme>> = OnceLock::new();
/// Whether the terminal background is light.  Set by the host binary at
/// startup so the crate can pick an adaptive default theme without depending
/// on platform-specific terminal-palette probing.
pub(super) static LIGHT_BG: OnceLock<Option<bool>> = OnceLock::new();

pub(super) fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}
