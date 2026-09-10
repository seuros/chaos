use super::*;
use crate::history_cell::HistoryCell;
use ratatui::text::Line;

struct Restore {
    appearance: Appearance,
    mode: ModeKind,
    clamped: bool,
}

impl Drop for Restore {
    fn drop(&mut self) {
        set_appearance(self.appearance.clone());
        set_collaboration_mode(self.mode);
        set_clamped(self.clamped);
    }
}

pub(crate) fn appearance_suite() {
    // This suite runs under libui's shared suite lock, like the other theme tests.
    let _restore = Restore {
        appearance: APPEARANCE.read().unwrap().clone(),
        mode: collaboration_mode(),
        clamped: is_clamped(),
    };
    terminal_colors();
    defaults_and_mode_overrides();
    role_styles_and_streaming();
}

fn terminal_colors() {
    for (value, expected) in [
        ("default", Color::Reset),
        ("black", Color::Black),
        ("red", Color::Red),
        ("green", Color::Green),
        ("yellow", Color::Yellow),
        ("blue", Color::Blue),
        ("magenta", Color::Magenta),
        ("cyan", Color::Cyan),
        ("gray", Color::Gray),
        ("dark_gray", Color::DarkGray),
        ("light_red", Color::LightRed),
        ("light_green", Color::LightGreen),
        ("light_yellow", Color::LightYellow),
        ("light_blue", Color::LightBlue),
        ("light_magenta", Color::LightMagenta),
        ("light_cyan", Color::LightCyan),
        ("white", Color::White),
        ("#aAbBcC", Color::Rgb(0xaa, 0xbb, 0xcc)),
        ("#000000", Color::Rgb(0, 0, 0)),
        ("#FFFFFF", Color::Rgb(255, 255, 255)),
    ] {
        let color = ThemeColor::try_from(value.to_owned()).unwrap();
        assert_eq!(terminal_color(&color), expected, "{value}");
    }
}

fn defaults_and_mode_overrides() {
    let overrides: Appearance =
        toml::from_str("[colors]\nfg = '#123456'\nborder = 'default'\naccent = 'light_blue'")
            .unwrap();
    for mode in [ModeKind::Default, ModeKind::Plan] {
        for clamped in [false, true] {
            let original = palette_for_mode(mode, clamped);
            assert_eq!(
                resolve_palette(original, &PaletteOverrides::default()),
                original
            );
            let resolved = resolve_palette(original, &overrides.colors);
            assert_eq!(resolved.fg, Color::Rgb(0x12, 0x34, 0x56));
            assert_eq!(resolved.border, Color::Reset);
            assert_eq!(resolved.accent, Color::LightBlue);
            assert_eq!(resolved.warning, original.warning);
            set_collaboration_mode(mode);
            set_clamped(clamped);
            set_appearance(overrides.clone());
            assert_eq!(palette(), resolved);
            assert_eq!(user_message(), text_panel());
        }
    }
    set_appearance(Appearance::default());
    assert_eq!(assistant_message(), Style::default());
    assert_eq!(user_message(), text_panel());

    let style: TextStyle = toml::from_str("bold = false\nitalic = false").unwrap();
    let resolved = resolve_text_style(
        Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
        &style,
    );
    assert!(
        !resolved
            .add_modifier
            .intersects(Modifier::BOLD | Modifier::ITALIC)
    );
    // Markdown can reintroduce emphasis after the base style has removed it.
    assert!(
        resolved
            .patch(Style::default().add_modifier(Modifier::BOLD))
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

fn find_style(lines: &[Line<'_>], content: &str) -> Style {
    for line in lines {
        for span in &line.spans {
            if span.content.contains(content) {
                return line.style.patch(span.style);
            }
        }
    }
    panic!("missing {content:?} in {lines:?}");
}

fn role_styles_and_streaming() {
    set_clamped(false);
    set_collaboration_mode(ModeKind::Default);
    set_appearance(Appearance::default());
    let panel_before = crate::style::text_panel_style();
    let cwd = std::env::temp_dir();
    let source = "Plain **strong** and *emphasis* with `inline_code`.\n\n```rust\nlet code_token = 1;\n```\n\n    indented_code\n";
    let mut original = Vec::new();
    crate::markdown::append_markdown(source, None, Some(&cwd), &mut original);

    set_appearance(toml::from_str("[user]\nfg = 'red'\nitalic = true").unwrap());
    assert_eq!(assistant_message(), Style::default());
    assert_eq!(crate::style::proposed_plan_style(), panel_before);
    let mut assistant_without_overrides = Vec::new();
    crate::markdown::append_markdown(source, None, Some(&cwd), &mut assistant_without_overrides);
    assert_eq!(assistant_without_overrides, original);

    set_appearance(
        toml::from_str(
            "[user]\nfg = 'red'\nbg = 'blue'\nbold = true\n\
         [assistant]\nfg = '#123456'\nbg = 'dark_gray'\nbold = false\nitalic = true",
        )
        .unwrap(),
    );
    assert_eq!(crate::style::text_panel_style(), panel_before);
    assert_eq!(crate::style::proposed_plan_style(), panel_before);
    assert_eq!(user_message().fg, Some(Color::Red));
    assert_eq!(assistant_message().fg, Some(Color::Rgb(0x12, 0x34, 0x56)));

    let user = crate::history_cell::new_user_prompt(
        "user body".into(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let user_lines = user.display_lines(80);
    let user_style = find_style(&user_lines, "user body");
    assert_eq!(user_style.fg, Some(Color::Red));
    assert!(user_style.add_modifier.contains(Modifier::BOLD));

    let mut rendered = Vec::new();
    crate::markdown::append_markdown(source, None, Some(&cwd), &mut rendered);
    let plain = find_style(&rendered, "Plain");
    assert_eq!(plain.fg, Some(Color::Rgb(0x12, 0x34, 0x56)));
    assert_eq!(plain.bg, Some(Color::DarkGray));
    assert!(plain.add_modifier.contains(Modifier::ITALIC));
    assert!(!plain.add_modifier.contains(Modifier::BOLD));
    assert!(
        find_style(&rendered, "strong")
            .add_modifier
            .contains(Modifier::BOLD)
    );
    for code in ["inline_code", "code_token", "indented_code"] {
        assert_eq!(find_style(&rendered, code), find_style(&original, code));
    }

    let mut collector = crate::markdown_stream::MarkdownStreamCollector::new(None, &cwd);
    let mut streamed = Vec::new();
    for delta in source.split_inclusive('\n') {
        collector.push_delta(delta);
        streamed.extend(collector.commit_complete_lines());
    }
    streamed.extend(collector.finalize_and_drain());
    assert_eq!(streamed, rendered);

    let reasoning = crate::history_cell::ReasoningSummaryCell::new(
        String::new(),
        "reasoning body".into(),
        &cwd,
        false,
    )
    .display_lines(80);
    let reasoning_style = find_style(&reasoning, "reasoning body");
    assert_eq!(reasoning_style.fg, plain.fg);
    assert!(
        reasoning_style
            .add_modifier
            .contains(Modifier::DIM | Modifier::ITALIC)
    );

    let plan = crate::history_cell::new_proposed_plan("plan body".into(), &cwd).display_lines(80);
    assert_eq!(find_style(&plan, "plan body").fg, plain.fg);

    // Reinstalling the final config replaces, rather than merges, old settings.
    set_appearance(Appearance::default());
    assert_eq!(user_message(), panel_before);
    let mut replayed = Vec::new();
    crate::markdown::append_markdown(source, None, Some(&cwd), &mut replayed);
    assert_eq!(replayed, original);
}
