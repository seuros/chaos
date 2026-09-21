use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Tabs, Widget};

use super::{AgentNavigationState, ProcessId};

impl AgentNavigationState {
    pub(crate) fn has_tabs(&self) -> bool {
        self.processes.len() > 1
    }

    pub(crate) fn tabs_contain(&self, position: Position) -> bool {
        self.tab_area.contains(position)
    }

    pub(crate) fn tab_at(&self, position: Position) -> Option<ProcessId> {
        self.tab_hits
            .iter()
            .find(|(_, area)| area.contains(position))
            .map(|(id, _)| *id)
    }

    pub(crate) fn render_tabs(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        active: Option<ProcessId>,
        primary: Option<ProcessId>,
    ) {
        self.tab_hits.clear();
        self.tab_area = Rect::ZERO;
        if area.is_empty() || !self.has_tabs() {
            return;
        }
        self.tab_area = area;
        let width = usize::from(area.width);
        let entries: Vec<_> = self
            .ordered_processes()
            .into_iter()
            .enumerate()
            .map(|(index, (id, entry))| {
                let label = self
                    .active_agent_label(Some(id), primary)
                    .unwrap_or_default();
                let label: String = label.chars().filter(|c| !c.is_control()).collect();
                let closed = if entry.is_closed { " ×" } else { "" };
                let title = libui::line_truncation::truncate_line_with_ellipsis_if_overflow(
                    Line::from(format!("{} {label}{closed}", index + 1)),
                    width.min(28),
                );
                (id, title)
            })
            .collect();
        let selected = entries
            .iter()
            .position(|(id, _)| Some(*id) == active)
            .unwrap_or(0);
        let mut first = 0;
        let mut used = entries[..=selected]
            .iter()
            .map(|(_, title)| title.width())
            .sum::<usize>()
            + selected * 3;
        while used > width && first < selected {
            used -= entries[first].1.width() + 3;
            first += 1;
        }
        let mut titles = Vec::new();
        let mut x = area.x;
        for (id, title) in entries.into_iter().skip(first) {
            let title_width = title.width() as u16;
            if title_width > area.right().saturating_sub(x) {
                break;
            }
            self.tab_hits
                .push((id, Rect::new(x, area.y, title_width, 1)));
            x = x.saturating_add(title_width).saturating_add(3);
            titles.push(title);
        }
        Tabs::new(titles)
            .padding("", "")
            .divider(" │ ")
            .select(selected - first)
            .style(crate::theme::dim())
            .highlight_style(crate::theme::highlight())
            .render(area, buf);
    }
}
