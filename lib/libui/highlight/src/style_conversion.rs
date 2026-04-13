use ratatui::style::Color as RtColor;
use ratatui::style::Modifier;
use ratatui::style::Style;
use syntect::highlighting::Color as SyntectColor;
use syntect::highlighting::FontStyle;
use syntect::highlighting::Style as SyntectStyle;

// Syntect/bat encode ANSI palette semantics in alpha:
// `a=0` => indexed ANSI palette via RGB payload, `a=1` => terminal default.
pub(super) const ANSI_ALPHA_INDEX: u8 = 0x00;
pub(super) const ANSI_ALPHA_DEFAULT: u8 = 0x01;
pub(super) const OPAQUE_ALPHA: u8 = 0xFF;

/// Map a low ANSI palette index (0-7) to ratatui's named color variants,
/// falling back to `Indexed(n)` for indices 8-255.
///
/// `clippy::disallowed_methods` is explicitly allowed here because this helper
/// intentionally constructs `ratatui::style::Color::Indexed`.
#[allow(clippy::disallowed_methods)]
pub(super) fn ansi_palette_color(index: u8) -> RtColor {
    match index {
        0x00 => RtColor::Black,
        0x01 => RtColor::Red,
        0x02 => RtColor::Green,
        0x03 => RtColor::Yellow,
        0x04 => RtColor::Blue,
        0x05 => RtColor::Magenta,
        0x06 => RtColor::Cyan,
        // ANSI code 37 is "white", represented as `Gray` in ratatui.
        0x07 => RtColor::Gray,
        n => RtColor::Indexed(n),
    }
}

/// Decode a syntect foreground `Color` into a ratatui color, respecting the
/// alpha-channel encoding that bat's `ansi`, `base16`, and `base16-256` themes
/// use to signal ANSI palette semantics instead of true RGB.
///
/// Returns `None` when the color signals "use the terminal's default
/// foreground", allowing the caller to omit the foreground attribute entirely.
///
/// `clippy::disallowed_methods` is explicitly allowed here because this helper
/// intentionally constructs `ratatui::style::Color::Rgb`.
#[allow(clippy::disallowed_methods)]
pub(super) fn convert_syntect_color(color: SyntectColor) -> Option<RtColor> {
    match color.a {
        ANSI_ALPHA_INDEX => Some(ansi_palette_color(color.r)),
        ANSI_ALPHA_DEFAULT => None,
        OPAQUE_ALPHA => Some(RtColor::Rgb(color.r, color.g, color.b)),
        _ => Some(RtColor::Rgb(color.r, color.g, color.b)),
    }
}

/// Convert a syntect `Style` to a ratatui `Style`.
///
/// Most themes produce RGB colors. The built-in `ansi`/`base16`/`base16-256`
/// themes encode ANSI palette semantics in the alpha channel, matching bat.
pub(super) fn convert_style(syn_style: SyntectStyle) -> Style {
    let mut rt_style = Style::default();

    if let Some(fg) = convert_syntect_color(syn_style.foreground) {
        rt_style = rt_style.fg(fg);
    }
    // Intentionally skip background to avoid overwriting terminal bg.
    // If background support is added later, decode with `convert_syntect_color`
    // to reuse the same alpha-marker semantics as foreground.

    if syn_style.font_style.contains(FontStyle::BOLD) {
        rt_style.add_modifier |= Modifier::BOLD;
    }
    // Intentionally skip italic -- many terminals render it poorly or not at all.
    // Intentionally skip underline -- themes like Dracula use underline on type
    // scopes (entity.name.type, support.class) which produces distracting
    // underlines on type/module names in terminal output.

    rt_style
}
