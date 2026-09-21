use ratatui::layout::{Constraint, Layout, Rect};

pub(super) struct FormLayout {
    pub title: Rect,
    pub fields: Vec<(usize, Rect, Rect)>,
    pub footer: Rect,
}

impl FormLayout {
    pub fn new(area: Rect, count: usize, focused: usize, footer_height: u16) -> Self {
        let [title, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(footer_height),
        ])
        .areas(area);
        let visible = usize::from(body.height / 2).min(count);
        let first = (focused + 1).saturating_sub(visible);
        let fields = Layout::vertical(vec![Constraint::Length(2); visible])
            .split(body)
            .iter()
            .enumerate()
            .map(|(offset, &area)| {
                let [label, input] = Layout::vertical([Constraint::Length(1); 2]).areas(area);
                (first + offset, label, input)
            })
            .collect();
        Self {
            title,
            fields,
            footer,
        }
    }
}
