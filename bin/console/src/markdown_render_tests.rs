use pretty_assertions::assert_eq;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use std::path::Path;

use crate::markdown_render::COLON_LOCATION_SUFFIX_RE;
use crate::markdown_render::HASH_LOCATION_SUFFIX_RE;
use crate::markdown_render::file_url_for_local_link;
use crate::markdown_render::render_markdown_text;
use crate::markdown_render::render_markdown_text_with_width_and_cwd;
use insta::assert_snapshot;

/// Span styled with the theme's "cyan" color (accent — used for inline code
/// and links in the renderer). Prefer this over `.cyan()` in tests so that
/// expectations track palette changes in one place.
fn accent(text: &'static str) -> Span<'static> {
    Span::styled(text, Style::new().fg(crate::theme::cyan()))
}

/// Span styled with the theme's "light_blue" color (dim — used for ordered
/// list markers). Prefer this over `.light_blue()` in tests.
fn dim_marker(text: &'static str) -> Span<'static> {
    Span::styled(text, Style::new().fg(crate::theme::light_blue()))
}

/// Span styled with accent color + underline (used for URL links).
fn accent_link(text: &'static str) -> Span<'static> {
    Span::styled(
        text,
        Style::new().fg(crate::theme::cyan()).underlined(),
    )
}

/// Span for a checked task-list marker (`[x] `): bold success-green.
fn task_checked(text: &'static str) -> Span<'static> {
    Span::styled(text, Style::new().fg(crate::theme::green()).bold())
}

/// Span for an unchecked task-list marker (`[ ] `): ANSI dim modifier,
/// no explicit foreground so it inherits the terminal base color.
fn task_unchecked(text: &'static str) -> Span<'static> {
    Span::styled(text, Style::new().dim())
}

/// Stamp the OSC 8 sentinel for `url` onto `span`.
fn linked(mut span: Span<'static>, url: &str) -> Span<'static> {
    span.style.underline_color = Some(crate::osc8::register(url));
    span
}

fn render_markdown_text_for_cwd(input: &str, cwd: &Path) -> Text<'static> {
    render_markdown_text_with_width_and_cwd(input, None, Some(cwd))
}

fn plain_lines(text: &Text<'_>) -> Vec<String> {
    text.lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn empty() {
    assert_eq!(render_markdown_text(""), Text::default());
}

#[test]
fn paragraph_single() {
    assert_eq!(
        render_markdown_text("Hello, world!"),
        Text::from("Hello, world!")
    );
}

#[test]
fn paragraph_soft_break() {
    assert_eq!(
        render_markdown_text("Hello\nWorld"),
        Text::from_iter(["Hello", "World"])
    );
}

#[test]
fn paragraph_multiple() {
    assert_eq!(
        render_markdown_text("Paragraph 1\n\nParagraph 2"),
        Text::from_iter(["Paragraph 1", "", "Paragraph 2"])
    );
}

#[test]
fn headings() {
    let md = "# Heading 1\n## Heading 2\n### Heading 3\n#### Heading 4\n##### Heading 5\n###### Heading 6\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["# ".bold().underlined(), "Heading 1".bold().underlined()]),
        Line::default(),
        Line::from_iter(["## ".bold(), "Heading 2".bold()]),
        Line::default(),
        Line::from_iter(["### ".bold().italic(), "Heading 3".bold().italic()]),
        Line::default(),
        Line::from_iter(["#### ".italic(), "Heading 4".italic()]),
        Line::default(),
        Line::from_iter(["##### ".italic(), "Heading 5".italic()]),
        Line::default(),
        Line::from_iter(["###### ".italic(), "Heading 6".italic()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_single() {
    let text = render_markdown_text("> Blockquote");
    let expected = Text::from(Line::from_iter(["> ", "Blockquote"]).green());
    assert_eq!(text, expected);
}

#[test]
fn blockquote_soft_break() {
    // Soft break via lazy continuation should render as a new line in blockquotes.
    let text = render_markdown_text("> This is a blockquote\nwith a soft break\n");
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "> This is a blockquote".to_string(),
            "> with a soft break".to_string()
        ]
    );
}

#[test]
fn blockquote_multiple_with_break() {
    let text = render_markdown_text("> Blockquote 1\n\n> Blockquote 2\n");
    let expected = Text::from_iter([
        Line::from_iter(["> ", "Blockquote 1"]).green(),
        Line::default(),
        Line::from_iter(["> ", "Blockquote 2"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_three_paragraphs_short_lines() {
    let md = "> one\n>\n> two\n>\n> three\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "one"]).green(),
        Line::from_iter(["> "]).green(),
        Line::from_iter(["> ", "two"]).green(),
        Line::from_iter(["> "]).green(),
        Line::from_iter(["> ", "three"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_nested_two_levels() {
    let md = "> Level 1\n>> Level 2\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "Level 1"]).green(),
        Line::from_iter(["> "]).green(),
        Line::from_iter(["> ", "> ", "Level 2"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_with_list_items() {
    let md = "> - item 1\n> - item 2\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "- ", "item 1"]).green(),
        Line::from_iter(["> ", "- ", "item 2"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_with_ordered_list() {
    let md = "> 1. first\n> 2. second\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(vec![
            Span::from("> "),
            dim_marker("1. "),
            Span::from("first"),
        ])
        .green(),
        Line::from_iter(vec![
            Span::from("> "),
            dim_marker("2. "),
            Span::from("second"),
        ])
        .green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_list_then_nested_blockquote() {
    let md = "> - parent\n>   > child\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "- ", "parent"]).green(),
        Line::from_iter(["> ", "  ", "> ", "child"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_item_with_inline_blockquote_on_same_line() {
    let md = "1. > quoted\n";
    let text = render_markdown_text(md);
    let mut lines = text.lines.iter();
    let first = lines.next().expect("one line");
    // Expect content to include the ordered marker, a space, "> ", and the text
    let s: String = first.spans.iter().map(|sp| sp.content.clone()).collect();
    assert_eq!(s, "1. > quoted");
}

#[test]
fn blockquote_surrounded_by_blank_lines() {
    let md = "foo\n\n> bar\n\nbaz\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "foo".to_string(),
            "".to_string(),
            "> bar".to_string(),
            "".to_string(),
            "baz".to_string(),
        ]
    );
}

#[test]
fn blockquote_in_ordered_list_on_next_line() {
    // Blockquote begins on a new line within an ordered list item; it should
    // render inline on the same marker line.
    let md = "1.\n   > quoted\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. > quoted".to_string()]);
}

#[test]
fn blockquote_in_unordered_list_on_next_line() {
    // Blockquote begins on a new line within an unordered list item; it should
    // render inline on the same marker line.
    let md = "-\n  > quoted\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- > quoted".to_string()]);
}

#[test]
fn blockquote_two_paragraphs_inside_ordered_list_has_blank_line() {
    // Two blockquote paragraphs inside a list item should be separated by a blank line.
    let md = "1.\n   > para 1\n   >\n   > para 2\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "1. > para 1".to_string(),
            "   > ".to_string(),
            "   > para 2".to_string(),
        ],
        "expected blockquote content to stay aligned after list marker"
    );
}

#[test]
fn blockquote_inside_nested_list() {
    let md = "1. A\n    - B\n      > inner\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. A", "    - B", "      > inner"]);
}

#[test]
fn list_item_text_then_blockquote() {
    let md = "1. before\n   > quoted\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. before", "   > quoted"]);
}

#[test]
fn list_item_blockquote_then_text() {
    let md = "1.\n   > quoted\n   after\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. > quoted", "   > after"]);
}

#[test]
fn list_item_text_blockquote_text() {
    let md = "1. before\n   > quoted\n   after\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. before", "   > quoted", "   > after"]);
}

#[test]
fn blockquote_with_heading_and_paragraph() {
    let md = "> # Heading\n> paragraph text\n";
    let text = render_markdown_text(md);
    // Validate on content shape; styling is handled elsewhere
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "> # Heading".to_string(),
            "> ".to_string(),
            "> paragraph text".to_string(),
        ]
    );
}

#[test]
fn blockquote_heading_inherits_heading_style() {
    let text = render_markdown_text("> # test header\n> in blockquote\n");
    assert_eq!(
        text.lines,
        [
            Line::from_iter([
                "> ".into(),
                "# ".bold().underlined(),
                "test header".bold().underlined(),
            ])
            .green(),
            Line::from_iter(["> "]).green(),
            Line::from_iter(["> ", "in blockquote"]).green(),
        ]
    );
}

#[test]
fn blockquote_with_code_block() {
    let md = "> ```\n> code\n> ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["> code".to_string()]);
}

#[test]
fn blockquote_with_multiline_code_block() {
    let md = "> ```\n> first\n> second\n> ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["> first", "> second"]);
}

#[test]
fn nested_blockquote_with_inline_and_fenced_code() {
    /*
    let md = \"> Nested quote with code:\n\
    > > Inner quote and `inline code`\n\
    > >\n\
    > > ```\n\
    > > # fenced code inside a quote\n\
    > > echo \"hello from a quote\"\n\
    > > ```\n";
    */
    let md = r#"> Nested quote with code:
> > Inner quote and `inline code`
> >
> > ```
> > # fenced code inside a quote
> > echo "hello from a quote"
> > ```
"#;
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "> Nested quote with code:".to_string(),
            "> ".to_string(),
            "> > Inner quote and inline code".to_string(),
            "> > ".to_string(),
            "> > # fenced code inside a quote".to_string(),
            "> > echo \"hello from a quote\"".to_string(),
        ]
    );
}

#[test]
fn list_unordered_single() {
    let text = render_markdown_text("- List item 1\n");
    let expected = Text::from_iter([Line::from_iter(["- ", "List item 1"])]);
    assert_eq!(text, expected);
}

#[test]
fn list_unordered_multiple() {
    let text = render_markdown_text("- List item 1\n- List item 2\n");
    let expected = Text::from_iter([
        Line::from_iter(["- ", "List item 1"]),
        Line::from_iter(["- ", "List item 2"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_ordered() {
    let text = render_markdown_text("1. List item 1\n2. List item 2\n");
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "List item 1".into()]),
        Line::from_iter([dim_marker("2. "), "List item 2".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_nested() {
    let text = render_markdown_text("- List item 1\n  - Nested list item 1\n");
    let expected = Text::from_iter([
        Line::from_iter(["- ", "List item 1"]),
        Line::from_iter(["    - ", "Nested list item 1"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_ordered_custom_start() {
    let text = render_markdown_text("3. First\n4. Second\n");
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("3. "), "First".into()]),
        Line::from_iter([dim_marker("4. "), "Second".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn nested_unordered_in_ordered() {
    let md = "1. Outer\n    - Inner A\n    - Inner B\n2. Next\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "Outer".into()]),
        Line::from_iter(["    - ", "Inner A"]),
        Line::from_iter(["    - ", "Inner B"]),
        Line::from_iter([dim_marker("2. "), "Next".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn nested_ordered_in_unordered() {
    let md = "- Outer\n    1. One\n    2. Two\n- Last\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["- ", "Outer"]),
        Line::from_iter([dim_marker("    1. "), "One".into()]),
        Line::from_iter([dim_marker("    2. "), "Two".into()]),
        Line::from_iter(["- ", "Last"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn loose_list_item_multiple_paragraphs() {
    let md = "1. First paragraph\n\n   Second paragraph of same item\n\n2. Next item\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "First paragraph".into()]),
        Line::default(),
        Line::from_iter(["   ", "Second paragraph of same item"]),
        Line::from_iter([dim_marker("2. "), "Next item".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn tight_item_with_soft_break() {
    let md = "- item line1\n  item line2\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["- ", "item line1"]),
        Line::from_iter(["  ", "item line2"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn deeply_nested_mixed_three_levels() {
    let md = "1. A\n    - B\n        1. C\n2. D\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "A".into()]),
        Line::from_iter(["    - ", "B"]),
        Line::from_iter([dim_marker("        1. "), "C".into()]),
        Line::from_iter([dim_marker("2. "), "D".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn loose_items_due_to_blank_line_between_items() {
    let md = "1. First\n\n2. Second\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "First".into()]),
        Line::from_iter([dim_marker("2. "), "Second".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn task_list_variants() {
    // One realistic checklist covering in a single pass:
    //   - checked state `[x]`
    //   - unchecked state `[ ]`
    //   - a loose list transition (blank line between items)
    //   - plain bullets coexisting with task items at the same level
    //   - checkbox-only items
    //   - nested task list under a plain parent (verifies the marker
    //     mutation applies to the innermost item on the indent stack)
    let md = "\
- [x] done

- [ ] todo
- plain sibling
- [x]
- [ ]
- parent
  - [x] nested done
  - [ ] nested todo
";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["- ".into(), task_checked("[x] "), "done".into()]),
        Line::from_iter(["- ".into(), task_unchecked("[ ] "), "todo".into()]),
        Line::from_iter(["- ", "plain sibling"]),
        Line::from_iter(["- ".into(), task_checked("[x] ")]),
        Line::from_iter(["- ".into(), task_unchecked("[ ] ")]),
        Line::from_iter(["- ", "parent"]),
        Line::from_iter([
            "    - ".into(),
            task_checked("[x] "),
            "nested done".into(),
        ]),
        Line::from_iter([
            "    - ".into(),
            task_unchecked("[ ] "),
            "nested todo".into(),
        ]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn task_list_wrapped_continuation_aligns_under_content() {
    // Width-constrained loose-list path: wrapped continuation must indent 6
    // cols so text sits flush under the item content (past `- [ ] `), not
    // under the bullet.
    let text = render_markdown_text_with_width_and_cwd(
        "- [ ] this item has text long enough to force a wrap\n\n- [x] next\n",
        Some(20),
        None,
    );
    assert!(text.lines.len() >= 3, "expected wrap: {text:?}");
    let first_line: String = text.lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        first_line.starts_with("- [ ] "),
        "expected checkbox marker on loose task item, got {first_line:?}"
    );
    let leading: String = text.lines[1]
        .spans
        .iter()
        .take_while(|s| s.content.chars().all(|c| c == ' '))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(
        leading.len(),
        6,
        "wrapped continuation should be indented 6 cols (past `- [ ] `), got {leading:?}"
    );
}

#[test]
fn mixed_tight_then_loose_in_one_list() {
    let md = "1. Tight\n\n2.\n   Loose\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "Tight".into()]),
        Line::from_iter([dim_marker("2. "), "Loose".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn ordered_item_with_indented_continuation_is_tight() {
    let md = "1. Foo\n   Bar\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "Foo".into()]),
        Line::from_iter(["   ", "Bar"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn inline_code() {
    let text = render_markdown_text("Example of `Inline code`");
    let expected = Line::from_iter(["Example of ".into(), accent("Inline code")]).into();
    assert_eq!(text, expected);
}

#[test]
fn strong() {
    assert_eq!(
        render_markdown_text("**Strong**"),
        Text::from(Line::from("Strong".bold()))
    );
}

#[test]
fn emphasis() {
    assert_eq!(
        render_markdown_text("*Emphasis*"),
        Text::from(Line::from("Emphasis".italic()))
    );
}

#[test]
fn strikethrough() {
    assert_eq!(
        render_markdown_text("~~Strikethrough~~"),
        Text::from(Line::from("Strikethrough".crossed_out()))
    );
}

#[test]
fn strong_emphasis() {
    let text = render_markdown_text("**Strong *emphasis***");
    let expected = Text::from(Line::from_iter([
        "Strong ".bold(),
        "emphasis".bold().italic(),
    ]));
    assert_eq!(text, expected);
}

#[test]
fn link() {
    let text = render_markdown_text("[Link](https://example.com)");
    let url = "https://example.com";
    // Every span emitted between `push_link` and `pop_link` — the label, the
    // parentheses, and the destination — picks up the OSC 8 sentinel so the
    // terminal treats the whole run as one clickable hyperlink.
    let expected = Text::from(Line::from_iter([
        linked("Link".into(), url),
        linked(" (".into(), url),
        linked(accent_link(url), url),
        linked(")".into(), url),
    ]));
    assert_eq!(text, expected);
}

#[test]
fn load_location_suffix_regexes() {
    let _colon = &*COLON_LOCATION_SUFFIX_RE;
    let _hash = &*HASH_LOCATION_SUFFIX_RE;
}

#[test]
fn file_link_hides_destination() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "/Users/example/code/chaos/chaos/tui/src/markdown_render.rs";
    let text = render_markdown_text_for_cwd(&format!("[chaos/tui/src/markdown_render.rs]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_appends_line_number_when_label_lacks_it() {
    let cwd = Path::new("/Users/example/code/chaos");
    // The display suffix `:74` is stripped when building the clickable URL —
    // terminals open `file://.../markdown_render.rs`, not `...:74`.
    let dest = "/Users/example/code/chaos/chaos/tui/src/markdown_render.rs:74";
    let text = render_markdown_text_for_cwd(&format!("[markdown_render.rs]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs:74"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_keeps_absolute_paths_outside_cwd() {
    let cwd = Path::new("/Users/example/code/chaos/chaos/tui");
    let dest = "/Users/example/code/chaos/README.md:74";
    let text = render_markdown_text_for_cwd(&format!("[README.md:74]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("/Users/example/code/chaos/README.md:74"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_appends_hash_anchor_when_label_lacks_it() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "file:///Users/example/code/chaos/chaos/tui/src/markdown_render.rs#L74C3";
    let text = render_markdown_text_for_cwd(&format!("[markdown_render.rs]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs:74:3"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_uses_target_path_for_hash_anchor() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "file:///Users/example/code/chaos/chaos/tui/src/markdown_render.rs#L74C3";
    let text = render_markdown_text_for_cwd(&format!("[markdown_render.rs#L74C3]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs:74:3"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_appends_range_when_label_lacks_it() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "/Users/example/code/chaos/chaos/tui/src/markdown_render.rs:74:3-76:9";
    let text = render_markdown_text_for_cwd(&format!("[markdown_render.rs]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs:74:3-76:9"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_uses_target_path_for_range() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "/Users/example/code/chaos/chaos/tui/src/markdown_render.rs:74:3-76:9";
    let text = render_markdown_text_for_cwd(&format!("[markdown_render.rs:74:3-76:9]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs:74:3-76:9"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_appends_hash_range_when_label_lacks_it() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "file:///Users/example/code/chaos/chaos/tui/src/markdown_render.rs#L74C3-L76C9";
    let text = render_markdown_text_for_cwd(&format!("[markdown_render.rs]({dest})"), cwd);
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs:74:3-76:9"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn multiline_file_link_label_after_styled_prefix_does_not_panic() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "file:///Users/example/code/chaos/chaos/tui/src/markdown_render.rs#L74C3";
    let text = render_markdown_text_for_cwd(
        &format!("**bold** plain [foo\nbar]({dest})"),
        cwd,
    );
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([
        "bold".bold(),
        " plain ".into(),
        linked(accent("chaos/tui/src/markdown_render.rs:74:3"), &url),
    ]));
    assert_eq!(text, expected);
}

#[test]
fn file_link_uses_target_path_for_hash_range() {
    let cwd = Path::new("/Users/example/code/chaos");
    let dest = "file:///Users/example/code/chaos/chaos/tui/src/markdown_render.rs#L74C3-L76C9";
    let text = render_markdown_text_for_cwd(
        &format!("[markdown_render.rs#L74C3-L76C9]({dest})"),
        cwd,
    );
    let url = file_url_for_local_link(dest, Some(cwd)).unwrap();
    let expected = Text::from(Line::from_iter([linked(
        accent("chaos/tui/src/markdown_render.rs:74:3-76:9"),
        &url,
    )]));
    assert_eq!(text, expected);
}

#[test]
fn file_url_for_local_link_preserves_percent_encoding() {
    let url = file_url_for_local_link("file:///tmp/My%20File.rs", None).unwrap();
    assert_eq!(url, "file:///tmp/My%20File.rs");
}

#[test]
fn file_url_for_local_link_rejects_windows_drive_paths() {
    let url = file_url_for_local_link(r"C:\tmp\My File.rs", None);
    assert_eq!(url, None);
}

#[test]
fn file_url_for_local_link_rejects_unc_paths() {
    let url = file_url_for_local_link(r"\\server\share\My File.rs", None);
    assert_eq!(url, None);
}

#[test]
fn url_link_shows_destination() {
    let text = render_markdown_text("[docs](https://example.com/docs)");
    let url = "https://example.com/docs";
    let expected = Text::from(Line::from_iter([
        linked("docs".into(), url),
        linked(" (".into(), url),
        linked(accent_link(url), url),
        linked(")".into(), url),
    ]));
    assert_eq!(text, expected);
}

#[test]
fn bare_url_autolink_does_not_duplicate_destination() {
    let text = render_markdown_text("Visit https://example.com/docs for details.");
    assert_eq!(
        plain_lines(&text),
        vec!["Visit https://example.com/docs for details.".to_string()]
    );
}

#[test]
fn bare_email_autolink_does_not_duplicate_destination() {
    let text = render_markdown_text("Email test@example.com for details.");
    assert_eq!(
        plain_lines(&text),
        vec!["Email test@example.com for details.".to_string()]
    );
}

#[test]
fn pipe_table_text_stays_verbatim() {
    let text = render_markdown_text(
        "| Left | Center | Right |\n|:-----|:------:|------:|\n| a | b | c |\n",
    );
    assert_eq!(
        plain_lines(&text),
        vec![
            "| Left | Center | Right |".to_string(),
            "|:-----|:------:|------:|".to_string(),
            "| a | b | c |".to_string(),
        ]
    );
}

#[test]
fn alert_blockquote_has_no_blank_line_after_header() {
    let text = render_markdown_text("> [!NOTE]\n> body\n");
    assert_eq!(
        plain_lines(&text),
        vec!["> ⓘ NOTE".to_string(), "> body".to_string()]
    );
}

#[test]
fn markdown_render_file_link_snapshot() {
    let text = render_markdown_text_for_cwd(
        "See [markdown_render.rs:74](/Users/example/code/chaos/chaos/tui/src/markdown_render.rs:74).",
        Path::new("/Users/example/code/chaos"),
    );
    let rendered = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!(rendered);
}

#[test]
fn unordered_list_local_file_link_stays_inline_with_following_text() {
    let text = render_markdown_text_with_width_and_cwd(
        "- [binary](/Users/example/code/chaos/chaos/README.md:93): core is the agent/business logic, tui is the terminal UI, exec is the headless automation surface, and cli is the top-level multitool binary.",
        Some(72),
        Some(Path::new("/Users/example/code/chaos")),
    );
    let rendered = text
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "- chaos/README.md:93: core is the agent/business logic, tui is the",
            "  terminal UI, exec is the headless automation surface, and cli is the",
            "  top-level multitool binary.",
        ]
    );
}

#[test]
fn unordered_list_local_file_link_soft_break_before_colon_stays_inline() {
    let text = render_markdown_text_with_width_and_cwd(
        "- [binary](/Users/example/code/chaos/chaos/README.md:93)\n  : core is the agent/business logic.",
        Some(72),
        Some(Path::new("/Users/example/code/chaos")),
    );
    let rendered = text
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec!["- chaos/README.md:93: core is the agent/business logic.",]
    );
}

#[test]
fn consecutive_unordered_list_local_file_links_do_not_detach_paths() {
    let text = render_markdown_text_with_width_and_cwd(
        "- [binary](/Users/example/code/chaos/chaos/README.md:93)\n  : cli is the top-level multitool binary.\n- [expectations](/Users/example/code/chaos/chaos/core/README.md:1)\n  : chaos-kern owns the real runtime behavior.",
        Some(72),
        Some(Path::new("/Users/example/code/chaos")),
    );
    let rendered = text
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "- chaos/README.md:93: cli is the top-level multitool binary.",
            "- chaos/core/README.md:1: chaos-kern owns the real runtime behavior.",
        ]
    );
}

#[test]
fn code_block_known_lang_has_syntax_colors() {
    let text = render_markdown_text("```rust\nfn main() {}\n```\n");
    let content: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    // Content should be preserved; ignore trailing empty line from highlighting.
    let content: Vec<&str> = content
        .iter()
        .map(std::string::String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(content, vec!["fn main() {}"]);

    // At least one span should have non-default style (syntax highlighting).
    let has_colored_span = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .any(|sp| sp.style.fg.is_some());
    assert!(has_colored_span, "expected syntax-highlighted spans with color");
}

#[test]
fn code_block_unknown_lang_plain() {
    let text = render_markdown_text("```xyzlang\nhello world\n```\n");
    let content: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    let content: Vec<&str> = content
        .iter()
        .map(std::string::String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(content, vec!["hello world"]);

    // No syntax coloring for unknown language — all spans have default style.
    let has_colored_span = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .any(|sp| sp.style.fg.is_some());
    assert!(!has_colored_span, "expected no syntax coloring for unknown lang");
}

#[test]
fn code_block_no_lang_plain() {
    let text = render_markdown_text("```\nno lang specified\n```\n");
    let content: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    let content: Vec<&str> = content
        .iter()
        .map(std::string::String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(content, vec!["no lang specified"]);
}

#[test]
fn code_block_multiple_lines_root() {
    let md = "```\nfirst\nsecond\n```\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["", "first"]),
        Line::from_iter(["", "second"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn code_block_indented() {
    let md = "    function greet() {\n      console.log(\"Hi\");\n    }\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["    ", "function greet() {"]),
        Line::from_iter(["    ", "  console.log(\"Hi\");"]),
        Line::from_iter(["    ", "}"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn horizontal_rule_renders_em_dashes() {
    let md = "Before\n\n---\n\nAfter\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["Before", "", "———", "", "After"]);
}

#[test]
fn code_block_with_inner_triple_backticks_outer_four() {
    let md = r#"````text
Here is a code block that shows another fenced block:

```md
# Inside fence
- bullet
- `inline code`
```
````
"#;
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    // Filter empty trailing lines for stability; the code block may or may
    // not emit a trailing blank depending on the highlighting path.
    let trimmed: Vec<&str> = {
        let mut v: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
        while v.last() == Some(&"") {
            v.pop();
        }
        v
    };
    assert_eq!(
        trimmed,
        vec![
            "Here is a code block that shows another fenced block:",
            "",
            "```md",
            "# Inside fence",
            "- bullet",
            "- `inline code`",
            "```",
        ]
    );
}

#[test]
fn code_block_inside_unordered_list_item_is_indented() {
    let md = "- Item\n\n  ```\n  code line\n  ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- Item", "", "  code line"]);
}

#[test]
fn code_block_multiple_lines_inside_unordered_list() {
    let md = "- Item\n\n  ```\n  first\n  second\n  ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- Item", "", "  first", "  second"]);
}

#[test]
fn code_block_inside_unordered_list_item_multiple_lines() {
    let md = "- Item\n\n  ```\n  first\n  second\n  ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- Item", "", "  first", "  second"]);
}

#[test]
fn markdown_render_complex_snapshot() {
    let md = r#"# H1: Markdown Streaming Test
Intro paragraph with bold **text**, italic *text*, and inline code `x=1`.
Combined bold-italic ***both*** and escaped asterisks \*literal\*.
Auto-link: <https://example.com> and reference link [ref][r1].
Link with title: [hover me](https://example.com "Example") and mailto <mailto:test@example.com>.
Image: ![alt text](https://example.com/img.png "Title")
> Blockquote level 1
>> Blockquote level 2 with `inline code`
- Unordered list item 1
  - Nested bullet with italics _inner_
- Unordered list item 2 with ~~strikethrough~~
1. Ordered item one
2. Ordered item two with sublist:
   1) Alt-numbered subitem
- [ ] Task: unchecked
- [x] Task: checked with link [home](https://example.org)
---
Table below (alignment test):
| Left | Center | Right |
|:-----|:------:|------:|
| a    |   b    |     c |
Inline HTML: <sup>sup</sup> and <sub>sub</sub>.
HTML block:
<div style="border:1px solid #ccc;padding:2px">inline block</div>
Escapes: \_underscores\_, backslash \\, ticks ``code with `backtick` inside``.
Emoji shortcodes: :sparkles: :tada: (if supported).
Hard break test (line ends with two spaces)  
Next line should be close to previous.
Footnote reference here[^1] and another[^longnote].
Horizontal rule with asterisks:
***
Fenced code block (JSON):
```json
{ "a": 1, "b": [true, false] }
```
Fenced code with tildes and triple backticks inside:
~~~markdown
To close ``` you need tildes.
~~~
Indented code block:
    for i in range(3): print(i)
Definition-like list:
Term
: Definition with `code`.
Character entities: &amp; &lt; &gt; &quot; &#39;
[^1]: This is the first footnote.
[^longnote]: A longer footnote with a link to [Rust](https://www.rust-lang.org/).
Escaped pipe in text: a \| b \| c.
URL with parentheses: [link](https://example.com/path_(with)_parens).
[r1]: https://example.com/ref "Reference link title"
"#;

    let text = render_markdown_text(md);
    // Convert to plain text lines for snapshot (ignore styles)
    let rendered = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!(rendered);
}

#[test]
fn ordered_item_with_code_block_and_nested_bullet() {
    let md = "1. **item 1**\n\n2. **item 2**\n   ```\n   code\n   ```\n   - `PROCESS_START` (a `OnceLock<Instant>`) keeps the start time for the entire process.\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "1. item 1".to_string(),
            "2. item 2".to_string(),
            String::new(),
            "   code".to_string(),
            "    - PROCESS_START (a OnceLock<Instant>) keeps the start time for the entire process.".to_string(),
        ]
    );
}

#[test]
fn nested_five_levels_mixed_lists() {
    let md = "1. First\n   - Second level\n     1. Third level (ordered)\n        - Fourth level (bullet)\n          - Fifth level to test indent consistency\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "First".into()]),
        Line::from_iter(["    - ", "Second level"]),
        Line::from_iter([dim_marker("        1. "), "Third level (ordered)".into()]),
        Line::from_iter(["            - ", "Fourth level (bullet)"]),
        Line::from_iter([
            "                - ",
            "Fifth level to test indent consistency",
        ]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn html_inline_is_verbatim() {
    let md = "Hello <span>world</span>!";
    let text = render_markdown_text(md);
    let expected: Text = Line::from_iter(["Hello ", "<span>", "world", "</span>", "!"]).into();
    assert_eq!(text, expected);
}

#[test]
fn html_block_is_verbatim_multiline() {
    let md = "<div>\n  <span>hi</span>\n</div>\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["<div>"]),
        Line::from_iter(["  <span>hi</span>"]),
        Line::from_iter(["</div>"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn html_in_tight_ordered_item_soft_breaks_with_space() {
    let md = "1. Foo\n   <i>Bar</i>\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "Foo".into()]),
        Line::from_iter(["   ", "<i>", "Bar", "</i>"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn html_continuation_paragraph_in_unordered_item_indented() {
    let md = "- Item\n\n  <em>continued</em>\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["- ", "Item"]),
        Line::default(),
        Line::from_iter(["  ", "<em>", "continued", "</em>"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn unordered_item_continuation_paragraph_is_indented() {
    let md = "- Intro\n\n  Continuation paragraph line 1\n  Continuation paragraph line 2\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "- Intro".to_string(),
            String::new(),
            "  Continuation paragraph line 1".to_string(),
            "  Continuation paragraph line 2".to_string(),
        ]
    );
}

#[test]
fn ordered_item_continuation_paragraph_is_indented() {
    let md = "1. Intro\n\n   More details about intro\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "Intro".into()]),
        Line::default(),
        Line::from_iter(["   ", "More details about intro"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn nested_item_continuation_paragraph_is_indented() {
    let md = "1. A\n    - B\n\n      Continuation for B\n2. C\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter([dim_marker("1. "), "A".into()]),
        Line::from_iter(["    - ", "B"]),
        Line::default(),
        Line::from_iter(["      ", "Continuation for B"]),
        Line::from_iter([dim_marker("2. "), "C".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn gfm_alerts_emit_styled_header_line_per_kind() {
    // One realistic document covering all five alert kinds so the test also
    // exercises the transitions between alerts.
    let md = "\
> [!NOTE]
> First

> [!TIP]
> Second

> [!IMPORTANT]
> Third

> [!WARNING]
> Fourth

> [!CAUTION]
> Fifth
";
    let text = render_markdown_text(md);
    let rendered: Vec<String> = text
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.clone()).collect::<String>())
        .collect();

    for needle in [
        "ⓘ NOTE",
        "★ TIP",
        "‼ IMPORTANT",
        "⚠ WARNING",
        "⛔ CAUTION",
    ] {
        assert!(
            rendered.iter().any(|l| l.contains(needle)),
            "alert header {needle:?} missing from: {rendered:?}"
        );
    }
    for body in ["First", "Second", "Third", "Fourth", "Fifth"] {
        assert!(
            rendered.iter().any(|l| l.contains(body)),
            "alert body {body:?} missing from: {rendered:?}"
        );
    }

    let p = crate::theme::palette();
    let expected = [
        ("NOTE", p.accent),
        ("TIP", p.success),
        ("IMPORTANT", p.accent),
        ("WARNING", p.warning),
        ("CAUTION", p.error),
    ];
    for (label, color) in expected {
        let styled = text.lines.iter().flat_map(|l| l.spans.iter()).any(|s| {
            s.content.contains(label)
                && s.style.fg == Some(color)
                && s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)
        });
        assert!(
            styled,
            "expected {label} header to be bold + fg {color:?}"
        );
    }
}

#[test]
fn inline_math_rewrites_latex_to_unicode() {
    // Inline math covers the real-world LLM payload: greek letters, operators,
    // set theory, blackboard bold, and the `^`/`_` superscript/subscript
    // notation that plain markdown's ENABLE_SUPERSCRIPT refused to parse.
    let text = render_markdown_text(
        "Energy: $E = mc^2$, inequality $x \\leq y$, greek $\\alpha + \\beta$, set $\\forall x \\in \\mathbb{R}$.\n",
    );
    let rendered: String = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
        .collect::<Vec<_>>()
        .join("");
    assert!(
        rendered.contains("E = mc²"),
        "expected superscript via math mode, got: {rendered:?}"
    );
    assert!(
        rendered.contains("x ≤ y"),
        "expected ≤ glyph, got: {rendered:?}"
    );
    assert!(
        rendered.contains("α + β"),
        "expected greek letters, got: {rendered:?}"
    );
    assert!(
        rendered.contains("∀ x ∈ ℝ"),
        "expected ∀/∈/ℝ, got: {rendered:?}"
    );
}

#[test]
fn display_math_rewrites_and_frames_on_own_lines() {
    // Display math ($$...$$) should rewrite glyphs and keep the normal single
    // paragraph gap before and after it.
    let text = render_markdown_text("Before.\n\n$$\\sum_{i=0}^{n} i^2$$\n\nAfter.\n");
    let rendered: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    let math_idx = rendered
        .iter()
        .position(|l| l.contains("∑"))
        .unwrap_or_else(|| panic!("expected ∑ line in: {rendered:?}"));
    assert!(
        rendered[math_idx].contains("ᵢ₌₀") && rendered[math_idx].contains('²'),
        "expected sub/superscripts in display math, got: {:?}",
        rendered[math_idx]
    );
    assert!(
        rendered.iter().any(|l| l == "Before."),
        "paragraph before display math missing: {rendered:?}"
    );
    assert!(
        rendered.iter().any(|l| l == "After."),
        "paragraph after display math missing: {rendered:?}"
    );
    assert_eq!(
        rendered,
        vec![
            "Before.".to_string(),
            "".to_string(),
            rendered[math_idx].clone(),
            "".to_string(),
            "After.".to_string(),
        ],
        "display math should not add an extra blank line: {rendered:?}"
    );
}

#[test]
fn inline_math_with_embedded_newline_splits_across_rendered_lines() {
    let text = render_markdown_text("Before $a\nb$ after.\n");
    let expected = Text::from_iter([
        Line::from_iter(vec![Span::from("Before "), accent("a")]),
        Line::from_iter(vec![accent("b"), Span::from(" after.")]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn display_math_with_embedded_newline_renders_each_line_once() {
    let text = render_markdown_text("Before.\n\n$$a\nb$$\n\nAfter.\n");
    let expected = Text::from_iter([
        Line::from("Before."),
        Line::default(),
        Line::from_iter([accent("a")]),
        Line::from_iter([accent("b")]),
        Line::default(),
        Line::from("After."),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn unknown_math_command_passes_through_unchanged() {
    // unicodeit leaves unknown commands intact rather than dropping them, so
    // math stays legible when a glyph mapping is missing.
    let text = render_markdown_text("Fraction: $\\frac{a}{b}$.\n");
    let rendered: String = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
        .collect::<Vec<_>>()
        .join("");
    assert!(
        rendered.contains("\\frac{a}{b}"),
        "expected \\frac passthrough, got: {rendered:?}"
    );
}

#[test]
fn code_block_preserves_trailing_blank_lines() {
    // A fenced code block with an intentional trailing blank line must keep it.
    let md = "```rust\nfn main() {}\n\n```\n";
    let text = render_markdown_text(md);
    let content: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    // Should have: "fn main() {}" then "" (the blank line).
    // Filter only to content lines (skip leading/trailing empty from rendering).
    assert!(
        content.iter().any(|c| c == "fn main() {}"),
        "expected code line, got {content:?}"
    );
    // The trailing blank line inside the fence should be preserved.
    let code_start = content.iter().position(|c| c == "fn main() {}").unwrap();
    assert!(
        content.len() > code_start + 1,
        "expected a line after 'fn main() {{}}' but content ends: {content:?}"
    );
    assert_eq!(
        content[code_start + 1], "",
        "trailing blank line inside code fence was lost: {content:?}"
    );
}
