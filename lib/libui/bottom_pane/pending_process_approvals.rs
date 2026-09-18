use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;

/// Widget that lists inactive processes with outstanding approval requests.
pub struct PendingProcessApprovals {
    processes: Vec<String>,
}

impl PendingProcessApprovals {
    pub fn new() -> Self {
        Self {
            processes: Vec::new(),
        }
    }

    pub fn set_processes(&mut self, processes: Vec<String>) -> bool {
        if self.processes == processes {
            return false;
        }
        self.processes = processes;
        true
    }

    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn processes(&self) -> &[String] {
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
                    .initial_indent(Line::from(vec![
                        "  ".into(),
                        "!".fg(crate::theme::error_color()).bold(),
                        " ".into(),
                    ]))
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
                "/agent".fg(crate::theme::accent_color()).bold(),
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
pub(crate) mod tests;
