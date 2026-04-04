use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;

/// Widget that lists inactive processes with outstanding approval requests.
pub(crate) struct PendingProcessApprovals {
    processes: Vec<String>,
}

impl PendingProcessApprovals {
    pub(crate) fn new() -> Self {
        Self {
            processes: Vec::new(),
        }
    }

    pub(crate) fn set_processes(&mut self, processes: Vec<String>) -> bool {
        if self.processes == processes {
            return false;
        }
        self.processes = processes;
        true
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn processes(&self) -> &[String] {
        &self.processes
    }

    fn as_renderable(&self, width: u16) -> Box<dyn Renderable> {
        if self.processes.is_empty() || width < 4 {
            return Box::new(());
        }

        let mut lines = Vec::new();
        for process in self.processes.iter().take(3) {
            let wrapped = adaptive_wrap_lines(
                std::iter::once(Line::from(format!("Approval needed in {process}"))),
                RtOptions::new(width as usize)
                    .initial_indent(Line::from(vec!["  ".into(), "!".red().bold(), " ".into()]))
                    .subsequent_indent(Line::from("    ")),
            );
            lines.extend(wrapped);
        }

        if self.processes.len() > 3 {
            lines.push(Line::from("    ...".dim().italic()));
        }

        lines.push(
            Line::from(vec![
                "    ".into(),
                "/agent".cyan().bold(),
                " to switch processes".dim(),
            ])
            .dim(),
        );

        Paragraph::new(lines).into()
    }
}

impl Renderable for PendingProcessApprovals {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        self.as_renderable(area.width).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable(width).desired_height(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;

    fn snapshot_rows(widget: &PendingProcessApprovals, width: u16) -> String {
        let height = widget.desired_height(width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        widget.render(Rect::new(0, 0, width, height), &mut buf);

        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn desired_height_empty() {
        let widget = PendingProcessApprovals::new();
        assert_eq!(widget.desired_height(40), 0);
    }

    #[test]
    fn render_single_process_snapshot() {
        let mut widget = PendingProcessApprovals::new();
        widget.set_processes(vec!["Robie [scout]".to_string()]);

        assert_snapshot!(
            snapshot_rows(&widget, 40).replace(' ', "."),
            @r"
..!.Approval.needed.in.Robie.[scout]....
..../agent.to.switch.processes.........."
        );
    }

    #[test]
    fn render_multiple_processes_snapshot() {
        let mut widget = PendingProcessApprovals::new();
        widget.set_processes(vec![
            "Main [default]".to_string(),
            "Robie [scout]".to_string(),
            "Inspector".to_string(),
            "Extra agent".to_string(),
        ]);

        assert_snapshot!(
            snapshot_rows(&widget, 44).replace(' ', "."),
            @r"
..!.Approval.needed.in.Main.[default].......
..!.Approval.needed.in.Robie.[scout]........
..!.Approval.needed.in.Inspector............
............................................
..../agent.to.switch.processes.............."
        );
    }
}
