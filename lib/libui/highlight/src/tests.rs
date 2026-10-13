use super::*;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::style::Color as RtColor;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use std::str::FromStr;
use syntect::highlighting::Color as SyntectColor;
use syntect::highlighting::FontStyle;
use syntect::highlighting::ScopeSelectors;
use syntect::highlighting::Style as SyntectStyle;
use syntect::highlighting::StyleModifier;
use syntect::highlighting::Theme;
use syntect::highlighting::ThemeItem;
use syntect::highlighting::ThemeSettings;

use two_face::theme::EmbeddedThemeName;

use crate::highlight_engine::{
    MAX_HIGHLIGHT_BYTES, MAX_HIGHLIGHT_LINES, highlight_to_line_spans_with_theme,
};
use crate::style_conversion::{ansi_palette_color, convert_style};
use crate::syntax_lookup::find_syntax;
use crate::theme_management::{
    diff_scope_background_rgbs_for_theme, parse_theme_name, parse_tmtheme, resolve_theme_by_name,
    set_custom_themes,
};
use std::collections::BTreeMap;
use std::sync::Once;

fn minimal_tmtheme() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>name</key><string>Test</string>
<key>settings</key><array><dict>
<key>settings</key><dict>
<key>foreground</key><string>#FFFFFF</string>
<key>background</key><string>#000000</string>
</dict></dict></array>
</dict></plist>"#
}

fn tmtheme_with_diff_backgrounds(
    inserted_scope: &str,
    inserted_background: &str,
    deleted_scope: &str,
    deleted_background: &str,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>name</key><string>Custom Diff Theme</string>
<key>settings</key><array>
<dict>
<key>settings</key><dict>
<key>foreground</key><string>#FFFFFF</string>
<key>background</key><string>#000000</string>
</dict>
</dict>
<dict>
<key>scope</key><string>{inserted_scope}</string>
<key>settings</key><dict>
<key>background</key><string>{inserted_background}</string>
</dict>
</dict>
<dict>
<key>scope</key><string>{deleted_scope}</string>
<key>settings</key><dict>
<key>background</key><string>{deleted_background}</string>
</dict>
</dict>
</array>
</dict></plist>"#
    )
}

/// Custom themes live in a process-wide `OnceLock`, so every test that needs
/// one shares a single installation containing all of them.
fn install_custom_themes() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let mut themes = BTreeMap::new();
        themes.insert(
            "custom-diff".to_string(),
            parse_tmtheme(&tmtheme_with_diff_backgrounds(
                "diff.inserted",
                "#102030",
                "markup.deleted",
                "#405060",
            ))
            .expect("diff theme parses"),
        );
        for name in [
            "zzz-custom",
            "Aaa-custom",
            "mmm-custom",
            "my-fancy",
            "dracula",
        ] {
            themes.insert(
                name.to_string(),
                parse_tmtheme(minimal_tmtheme()).expect("minimal theme parses"),
            );
        }
        set_custom_themes(themes);
    });
}

/// Reconstruct plain text from highlighted Lines.
fn reconstructed(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|sp| sp.content.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn unique_foreground_colors_for_theme(theme_name: &str) -> Vec<String> {
    let theme = resolve_theme_by_name(theme_name)
        .unwrap_or_else(|| panic!("expected built-in theme {theme_name} to resolve"));
    let lines = highlight_to_line_spans_with_theme(
        "fn main() { let answer = 42; println!(\"hello\"); }\n",
        "rust",
        &theme,
    )
    .expect("expected highlighted spans");
    let mut colors: Vec<String> = lines
        .iter()
        .flat_map(|line| line.iter().filter_map(|span| span.style.fg))
        .map(|fg| format!("{fg:?}"))
        .collect();
    colors.sort();
    colors.dedup();
    colors
}

fn theme_item(scope: &str, background: Option<(u8, u8, u8)>) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scope).expect("scope selector should parse"),
        style: StyleModifier {
            background: background.map(|(r, g, b)| SyntectColor { r, g, b, a: 255 }),
            ..StyleModifier::default()
        },
    }
}

#[test]
fn highlight_rust_has_keyword_style() {
    let code = "fn main() {}";
    let lines = highlight_code_to_lines(code, "rust");
    assert_eq!(reconstructed(&lines), code);

    let fn_span = lines[0].spans.iter().find(|sp| sp.content.as_ref() == "fn");
    assert!(fn_span.is_some(), "expected a span containing 'fn'");
    let style = fn_span.map(|s| s.style).unwrap_or_default();
    assert!(
        style.fg.is_some() || style.add_modifier != Modifier::empty(),
        "expected fn keyword to have non-default style, got {style:?}"
    );
}

#[test]
fn highlight_unknown_lang_falls_back() {
    let code = "some random text";
    let lines = highlight_code_to_lines(code, "xyzlang");
    assert_eq!(reconstructed(&lines), code);
    for line in &lines {
        for span in &line.spans {
            assert_eq!(
                span.style,
                Style::default(),
                "expected default style for unknown language"
            );
        }
    }
}

#[test]
fn fallback_trailing_newline_no_phantom_line() {
    let code = "hello world\n";
    let lines = highlight_code_to_lines(code, "xyzlang");
    assert_eq!(
        lines.len(),
        1,
        "trailing newline should not produce phantom blank line, got {lines:?}"
    );
    assert_eq!(reconstructed(&lines), "hello world");
}

#[test]
fn highlight_empty_string() {
    let lines = highlight_code_to_lines("", "rust");
    assert_eq!(lines.len(), 1);
    assert_eq!(reconstructed(&lines), "");
}

#[test]
fn highlight_bash_preserves_content() {
    let script = "echo \"hello world\" && ls -la | grep foo";
    let lines = highlight_bash_to_lines(script);
    assert_eq!(reconstructed(&lines), script);
}

#[test]
fn highlight_crlf_strips_carriage_return() {
    let code = "fn main() {\r\n    println!(\"hi\");\r\n}\r\n";
    let lines = highlight_code_to_lines(code, "rust");
    for (i, line) in lines.iter().enumerate() {
        for span in &line.spans {
            assert!(
                !span.content.contains('\r'),
                "line {i} span {:?} contains \\r",
                span.content,
            );
        }
    }
}

#[test]
#[allow(clippy::disallowed_methods)]
fn style_conversion_correctness() {
    let syn = SyntectStyle {
        foreground: syntect::highlighting::Color {
            r: 255,
            g: 128,
            b: 0,
            a: 255,
        },
        background: syntect::highlighting::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        },
        font_style: FontStyle::BOLD | FontStyle::ITALIC,
    };
    let rt = convert_style(syn);
    assert_eq!(rt.fg, Some(RtColor::Rgb(255, 128, 0)));
    assert_eq!(rt.bg, None);
    assert!(rt.add_modifier.contains(Modifier::BOLD));
    assert!(!rt.add_modifier.contains(Modifier::ITALIC));
    assert!(!rt.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn convert_style_suppresses_underline() {
    let syn = SyntectStyle {
        foreground: syntect::highlighting::Color {
            r: 100,
            g: 200,
            b: 150,
            a: 255,
        },
        background: syntect::highlighting::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0xFF,
        },
        font_style: FontStyle::UNDERLINE,
    };
    let rt = convert_style(syn);
    assert!(
        !rt.add_modifier.contains(Modifier::UNDERLINED),
        "convert_style should suppress UNDERLINE from themes -- \
             themes like Dracula use underline on type scopes which \
             looks wrong in terminal output"
    );
}

#[test]
fn style_conversion_uses_ansi_named_color_when_alpha_is_zero_low_index() {
    let syn = SyntectStyle {
        foreground: syntect::highlighting::Color {
            r: 0x02,
            g: 0,
            b: 0,
            a: 0,
        },
        background: syntect::highlighting::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0xFF,
        },
        font_style: FontStyle::empty(),
    };
    let rt = convert_style(syn);
    assert_eq!(rt.fg, Some(RtColor::Green));
}

#[test]
fn style_conversion_uses_indexed_color_when_alpha_is_zero_high_index() {
    let syn = SyntectStyle {
        foreground: syntect::highlighting::Color {
            r: 0x9a,
            g: 0,
            b: 0,
            a: 0,
        },
        background: syntect::highlighting::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0xFF,
        },
        font_style: FontStyle::empty(),
    };
    let rt = convert_style(syn);
    std::assert_matches!(rt.fg, Some(RtColor::Indexed(0x9a)));
}

#[test]
fn style_conversion_uses_terminal_default_when_alpha_is_one() {
    let syn = SyntectStyle {
        foreground: syntect::highlighting::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 1,
        },
        background: syntect::highlighting::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0xFF,
        },
        font_style: FontStyle::empty(),
    };
    let rt = convert_style(syn);
    assert_eq!(rt.fg, None);
}

#[test]
fn style_conversion_unexpected_alpha_falls_back_to_rgb() {
    let syn = SyntectStyle {
        foreground: syntect::highlighting::Color {
            r: 10,
            g: 20,
            b: 30,
            a: 0x80,
        },
        background: syntect::highlighting::Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0xFF,
        },
        font_style: FontStyle::empty(),
    };
    let rt = convert_style(syn);
    std::assert_matches!(rt.fg, Some(RtColor::Rgb(10, 20, 30)));
}

#[test]
fn ansi_palette_color_maps_ansi_white_to_gray() {
    assert_eq!(ansi_palette_color(0x07), RtColor::Gray);
}

#[test]
fn ansi_family_themes_use_terminal_palette_colors_not_rgb() {
    for theme_name in ["ansi", "base16", "base16-256"] {
        let theme = resolve_theme_by_name(theme_name)
            .unwrap_or_else(|| panic!("expected built-in theme {theme_name} to resolve"));
        let lines = highlight_to_line_spans_with_theme(
            "fn main() { let answer = 42; println!(\"hello\"); }\n",
            "rust",
            &theme,
        )
        .expect("expected highlighted spans");
        let mut has_non_default_fg = false;
        for line in &lines {
            for span in line {
                match span.style.fg {
                    Some(RtColor::Rgb(..)) => {
                        panic!("theme {theme_name} produced RGB foreground: {span:?}")
                    }
                    Some(_) => has_non_default_fg = true,
                    None => {}
                }
            }
        }
        assert!(
            has_non_default_fg,
            "theme {theme_name} should produce at least one non-default foreground color"
        );
    }
}

#[test]
fn ansi_family_foreground_palette_snapshot() {
    let mut out = String::new();
    for theme_name in ["ansi", "base16", "base16-256"] {
        let colors = unique_foreground_colors_for_theme(theme_name);
        out.push_str(&format!("{theme_name}:\n"));
        for color in colors {
            out.push_str(&format!("  {color}\n"));
        }
    }
    assert_snapshot!("ansi_family_foreground_palette", out);
}

#[test]
fn highlight_multiline_python() {
    let code = "def hello():\n    print(\"hi\")\n    return 42";
    let lines = highlight_code_to_lines(code, "python");
    assert_eq!(reconstructed(&lines), code);
    assert_eq!(lines.len(), 3);
}

#[test]
fn highlight_code_to_styled_spans_returns_none_for_unknown() {
    assert!(highlight_code_to_styled_spans("x", "xyzlang").is_none());
}

#[test]
fn highlight_code_to_styled_spans_returns_some_for_known() {
    let result = highlight_code_to_styled_spans("let x = 1;", "rust");
    assert!(result.is_some());
    let spans = result.unwrap_or_default();
    assert!(!spans.is_empty());
}

#[test]
fn highlight_markdown_preserves_content() {
    let code = "```sh\nprintf 'fenced within fenced\\n'\n```";
    let lines = highlight_code_to_lines(code, "markdown");
    let result = reconstructed(&lines);
    assert_eq!(
        result, code,
        "markdown highlighting must preserve content exactly"
    );
}

#[test]
fn highlight_large_input_falls_back() {
    let big = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
    let result = highlight_code_to_styled_spans(&big, "rust");
    assert!(result.is_none(), "oversized input should fall back to None");
}

#[test]
fn highlight_many_lines_falls_back() {
    let many_lines = "let x = 1;\n".repeat(MAX_HIGHLIGHT_LINES + 1);
    let result = highlight_code_to_styled_spans(&many_lines, "rust");
    assert!(result.is_none(), "too many lines should fall back to None");
}

#[test]
fn highlight_many_lines_no_trailing_newline_falls_back() {
    let mut code = "let x = 1;\n".repeat(MAX_HIGHLIGHT_LINES);
    code.push_str("let x = 1;");
    assert_eq!(code.lines().count(), MAX_HIGHLIGHT_LINES + 1);
    let result = highlight_code_to_styled_spans(&code, "rust");
    assert!(
        result.is_none(),
        "MAX_HIGHLIGHT_LINES+1 lines without trailing newline should fall back"
    );
}

#[test]
fn find_syntax_resolves_languages_and_aliases() {
    let languages = [
        "javascript",
        "typescript",
        "tsx",
        "python",
        "ruby",
        "rust",
        "go",
        "c",
        "cpp",
        "yaml",
        "bash",
        "kotlin",
        "markdown",
        "sql",
        "lua",
        "zig",
        "swift",
        "java",
        "c#",
        "elixir",
        "haskell",
        "scala",
        "dart",
        "r",
        "perl",
        "php",
        "html",
        "css",
        "json",
        "toml",
        "xml",
        "dockerfile",
    ];
    for lang in languages {
        assert!(
            find_syntax(lang).is_some(),
            "find_syntax({lang:?}) returned None"
        );
    }
    let extensions = [
        "rs", "py", "js", "ts", "rb", "go", "sh", "md", "yml", "kt", "ex", "hs", "pl", "php",
        "css", "html", "cs",
    ];
    for ext in extensions {
        assert!(
            find_syntax(ext).is_some(),
            "find_syntax({ext:?}) returned None"
        );
    }
    for alias in ["csharp", "c-sharp", "golang", "python3", "shell"] {
        assert!(
            find_syntax(alias).is_some(),
            "find_syntax({alias:?}) returned None -- patched alias broken"
        );
    }
}

#[test]
fn diff_scope_backgrounds_prefer_markup_scope_then_diff_fallback() {
    let theme = Theme {
        settings: ThemeSettings::default(),
        scopes: vec![
            theme_item("markup.inserted", Some((10, 20, 30))),
            theme_item("diff.deleted", Some((40, 50, 60))),
        ],
        ..Theme::default()
    };
    let rgbs = diff_scope_background_rgbs_for_theme(&theme);
    assert_eq!(
        rgbs,
        DiffScopeBackgroundRgbs {
            inserted: Some((10, 20, 30)),
            deleted: Some((40, 50, 60)),
        }
    );
}

#[test]
fn diff_scope_backgrounds_return_none_when_no_background_scope_matches() {
    let theme = Theme {
        settings: ThemeSettings::default(),
        scopes: vec![theme_item("constant.numeric", Some((1, 2, 3)))],
        ..Theme::default()
    };
    let rgbs = diff_scope_background_rgbs_for_theme(&theme);
    assert_eq!(
        rgbs,
        DiffScopeBackgroundRgbs {
            inserted: None,
            deleted: None,
        }
    );
}

#[test]
fn bundled_theme_can_provide_diff_scope_backgrounds() {
    let theme = resolve_theme_by_name("github").expect("expected built-in GitHub theme to load");
    let rgbs = diff_scope_background_rgbs_for_theme(&theme);
    assert!(
        rgbs.inserted.is_some() && rgbs.deleted.is_some(),
        "expected built-in theme to provide insert/delete backgrounds, got {rgbs:?}"
    );
}

#[test]
fn custom_tmtheme_diff_scope_backgrounds_are_resolved() {
    install_custom_themes();
    let theme = resolve_theme_by_name("custom-diff").expect("expected custom theme to resolve");
    let rgbs = diff_scope_background_rgbs_for_theme(&theme);
    assert_eq!(
        rgbs,
        DiffScopeBackgroundRgbs {
            inserted: Some((16, 32, 48)),
            deleted: Some((64, 80, 96)),
        }
    );
}

#[test]
fn parse_theme_name_covers_all_variants() {
    let known = [
        ("ansi", EmbeddedThemeName::Ansi),
        ("base16", EmbeddedThemeName::Base16),
        (
            "base16-eighties-dark",
            EmbeddedThemeName::Base16EightiesDark,
        ),
        ("base16-mocha-dark", EmbeddedThemeName::Base16MochaDark),
        ("base16-ocean-dark", EmbeddedThemeName::Base16OceanDark),
        ("base16-ocean-light", EmbeddedThemeName::Base16OceanLight),
        ("base16-256", EmbeddedThemeName::Base16_256),
        ("catppuccin-frappe", EmbeddedThemeName::CatppuccinFrappe),
        ("catppuccin-latte", EmbeddedThemeName::CatppuccinLatte),
        (
            "catppuccin-macchiato",
            EmbeddedThemeName::CatppuccinMacchiato,
        ),
        ("catppuccin-mocha", EmbeddedThemeName::CatppuccinMocha),
        ("coldark-cold", EmbeddedThemeName::ColdarkCold),
        ("coldark-dark", EmbeddedThemeName::ColdarkDark),
        ("dark-neon", EmbeddedThemeName::DarkNeon),
        ("dracula", EmbeddedThemeName::Dracula),
        ("github", EmbeddedThemeName::Github),
        ("gruvbox-dark", EmbeddedThemeName::GruvboxDark),
        ("gruvbox-light", EmbeddedThemeName::GruvboxLight),
        ("inspired-github", EmbeddedThemeName::InspiredGithub),
        ("1337", EmbeddedThemeName::Leet),
        ("monokai-extended", EmbeddedThemeName::MonokaiExtended),
        (
            "monokai-extended-bright",
            EmbeddedThemeName::MonokaiExtendedBright,
        ),
        (
            "monokai-extended-light",
            EmbeddedThemeName::MonokaiExtendedLight,
        ),
        (
            "monokai-extended-origin",
            EmbeddedThemeName::MonokaiExtendedOrigin,
        ),
        ("nord", EmbeddedThemeName::Nord),
        ("one-half-dark", EmbeddedThemeName::OneHalfDark),
        ("one-half-light", EmbeddedThemeName::OneHalfLight),
        ("solarized-dark", EmbeddedThemeName::SolarizedDark),
        ("solarized-light", EmbeddedThemeName::SolarizedLight),
        ("sublime-snazzy", EmbeddedThemeName::SublimeSnazzy),
        ("two-dark", EmbeddedThemeName::TwoDark),
        ("zenburn", EmbeddedThemeName::Zenburn),
    ];
    for (kebab, expected) in &known {
        assert_eq!(
            parse_theme_name(kebab),
            Some(*expected),
            "parse_theme_name({kebab:?}) did not return expected variant"
        );
    }
}

#[test]
fn parse_theme_name_returns_none_for_unknown() {
    assert_eq!(parse_theme_name("nonexistent-theme"), None);
    assert_eq!(parse_theme_name(""), None);
}

#[test]
fn parse_tmtheme_accepts_valid_and_rejects_invalid_documents() {
    assert!(parse_tmtheme(minimal_tmtheme()).is_some());
    assert!(parse_tmtheme("placeholder").is_none());
    assert!(parse_tmtheme("not a plist").is_none());
}

#[test]
fn resolve_theme_by_name_returns_none_for_unknown() {
    install_custom_themes();
    assert!(resolve_theme_by_name("nonexistent").is_none());
}

#[test]
fn validate_theme_name_none_for_bundled() {
    assert!(validate_theme_name(Some("dracula")).is_none());
    assert!(validate_theme_name(Some("nord")).is_none());
}

#[test]
fn validate_theme_name_none_when_no_override() {
    assert!(validate_theme_name(None).is_none());
}

#[test]
fn validate_theme_name_warns_for_unknown_with_import_hint() {
    install_custom_themes();
    let warning = validate_theme_name(Some("not-installed")).expect("unknown theme warns");
    assert!(warning.contains("not-installed"), "{warning}");
    assert!(warning.contains("chaos themes import"), "{warning}");
}

#[test]
fn validate_theme_name_none_when_custom_theme_is_installed() {
    install_custom_themes();
    assert!(
        validate_theme_name(Some("my-fancy")).is_none(),
        "should not warn when a stored custom theme is installed"
    );
}

#[test]
fn builtin_shadows_custom_theme_with_same_name() {
    install_custom_themes();
    let entries = list_available_themes();
    let dracula: Vec<_> = entries.iter().filter(|e| e.name == "dracula").collect();
    assert_eq!(dracula.len(), 1);
    assert!(
        !dracula[0].is_custom,
        "built-in dracula wins over the custom one"
    );
}

#[test]
fn list_available_themes_includes_installed_custom_themes() {
    install_custom_themes();
    let entries = list_available_themes();
    for name in ["custom-diff", "my-fancy", "zzz-custom"] {
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == name && entry.is_custom),
            "expected custom theme {name} to be listed"
        );
    }
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    let mut deduped = names.clone();
    deduped.dedup();
    assert_eq!(names, deduped, "no duplicate theme names");
}

#[test]
fn list_available_themes_returns_stable_sorted_order() {
    install_custom_themes();
    let entries = list_available_themes();
    let actual: Vec<(bool, String)> = entries
        .iter()
        .map(|entry| (entry.is_custom, entry.name.clone()))
        .collect();

    let mut expected = actual.clone();
    expected.sort_by_cached_key(|entry| (entry.1.to_ascii_lowercase(), entry.1.clone()));

    assert_eq!(
        actual, expected,
        "theme entries should be stable and sorted case-insensitively across built-in and custom themes"
    );
}

#[test]
fn parse_theme_name_is_exhaustive() {
    use two_face::theme::EmbeddedLazyThemeSet;

    let all_variants = EmbeddedLazyThemeSet::theme_names();

    assert_eq!(
        all_variants.len(),
        32,
        "two-face theme count changed -- update parse_theme_name"
    );

    let kebab_names = [
        "ansi",
        "base16",
        "base16-eighties-dark",
        "base16-mocha-dark",
        "base16-ocean-dark",
        "base16-ocean-light",
        "base16-256",
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
        "1337",
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
    let mapped: Vec<EmbeddedThemeName> = kebab_names
        .iter()
        .map(|k| parse_theme_name(k).unwrap_or_else(|| panic!("unmapped kebab name: {k}")))
        .collect();

    for variant in all_variants {
        assert!(
            mapped.contains(variant),
            "EmbeddedThemeName::{variant:?} has no kebab-case mapping in parse_theme_name"
        );
    }
}
