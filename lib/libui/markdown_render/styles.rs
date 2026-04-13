use ratatui::style::Style;

pub(super) struct MarkdownStyles {
    pub(super) h1: Style,
    pub(super) h2: Style,
    pub(super) h3: Style,
    pub(super) h4: Style,
    pub(super) h5: Style,
    pub(super) h6: Style,
    pub(super) code: Style,
    pub(super) emphasis: Style,
    pub(super) strong: Style,
    pub(super) strikethrough: Style,
    pub(super) ordered_list_marker: Style,
    pub(super) unordered_list_marker: Style,
    pub(super) link: Style,
    pub(super) blockquote: Style,
    pub(super) task_checked: Style,
    pub(super) task_unchecked: Style,
}

impl Default for MarkdownStyles {
    fn default() -> Self {
        let p = crate::theme::palette();
        Self {
            // Headings inherit the terminal foreground rather than setting an
            // explicit colour. The base fg already is the phosphor green, so
            // an explicit fg(p.fg) would be visually redundant and would
            // break equality checks in tests. Modifiers alone carry the
            // visual hierarchy. H6 gets dim to distinguish it from H5.
            h1: Style::new().bold().underlined(),
            h2: Style::new().bold(),
            h3: Style::new().bold().italic(),
            h4: Style::new().italic(),
            h5: Style::new().italic(),
            // H6 also inherits the terminal fg — see note above.
            h6: Style::new().italic(),
            code: Style::new().fg(p.accent),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().crossed_out(),
            ordered_list_marker: Style::new().fg(p.dim),
            unordered_list_marker: Style::new(),
            link: Style::new().fg(p.accent).underlined(),
            blockquote: Style::new().fg(p.success),
            // Task-list checkboxes. We intentionally use ANSI modifiers
            // (bold / dim) rather than fg colors because the phosphor theme
            // collapses both `dim` and `success` to `Color::Green`, making
            // any color-only contrast invisible. Bold vs dim survives
            // monochrome themes and still distinguishes the two states on a
            // full-color terminal. Item text itself stays unstyled — a
            // completed-item strikethrough would collide with the regular
            // `Strikethrough` event.
            task_checked: Style::new().fg(p.success).bold(),
            task_unchecked: Style::new().dim(),
        }
    }
}
