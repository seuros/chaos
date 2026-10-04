use std::path::Path;
use std::path::PathBuf;

use crate::markdown::append_markdown;
use crate::render::line_utils::prefix_lines;
use crate::render::line_utils::push_owned_lines;
use crate::style::proposed_plan_style;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use chaos_ipc::plan_tool::{PlanTask, PlanUpdate, TaskStatus};
use ratatui::prelude::*;
use ratatui::style::Style;
use ratatui::style::Styled;
use ratatui::style::Stylize;

use super::trait_def::HistoryCell;

// ---------------------------------------------------------------------------
// ProposedPlanCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ProposedPlanCell {
    plan_markdown: String,
    /// Session cwd used to keep local file-link display aligned with live streamed plan rendering.
    cwd: PathBuf,
}

impl HistoryCell for ProposedPlanCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(vec!["• ".dim(), "Proposed Plan".bold()].into());
        lines.push(Line::from(" "));

        let mut plan_lines: Vec<Line<'static>> = vec![Line::from(" ")];
        let plan_style = proposed_plan_style();
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let mut body: Vec<Line<'static>> = Vec::new();
        append_markdown(
            &self.plan_markdown,
            Some(wrap_width),
            Some(self.cwd.as_path()),
            &mut body,
        );
        if body.is_empty() {
            body.push(Line::from("(empty)".dim().italic()));
        }
        plan_lines.extend(prefix_lines(body, "  ".into(), "  ".into()));
        plan_lines.push(Line::from(" "));

        lines.extend(plan_lines.into_iter().map(|line| line.style(plan_style)));
        lines
    }
}

/// Create a proposed-plan cell that snapshots the session cwd for later markdown rendering.
pub fn new_proposed_plan(plan_markdown: String, cwd: &Path) -> ProposedPlanCell {
    ProposedPlanCell {
        plan_markdown,
        cwd: cwd.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// ProposedPlanStreamCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ProposedPlanStreamCell {
    lines: Vec<Line<'static>>,
    is_stream_continuation: bool,
}

impl HistoryCell for ProposedPlanStreamCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_stream_continuation
    }
}

pub fn new_proposed_plan_stream(
    lines: Vec<Line<'static>>,
    is_stream_continuation: bool,
) -> ProposedPlanStreamCell {
    ProposedPlanStreamCell {
        lines,
        is_stream_continuation,
    }
}

// ---------------------------------------------------------------------------
// PlanUpdateCell
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PlanUpdateCell {
    update: PlanUpdate,
}

impl HistoryCell for PlanUpdateCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let render_task = |task: &PlanTask| -> Vec<Line<'static>> {
            let (marker, style) = match task.status {
                TaskStatus::Completed => ("✔ ", Style::default().crossed_out().dim()),
                TaskStatus::Cancelled => ("× ", Style::default().crossed_out().dim()),
                TaskStatus::Blocked => ("! ", Style::default().yellow()),
                TaskStatus::InProgress => (
                    "▶ ",
                    Style::default().fg(crate::theme::accent_color()).bold(),
                ),
                TaskStatus::Pending => ("□ ", Style::default().dim()),
            };
            // Cap only visual indentation to the viewport, not the stored hierarchy.
            let indent =
                " ".repeat((task.depth as usize * 2).min(width.saturating_sub(12) as usize));
            let opts = RtOptions::new(width.saturating_sub(4).max(1) as usize)
                .initial_indent(format!("{indent}{marker}").into())
                .subsequent_indent(format!("{indent}  ").into());
            let task = Line::from(format!("{} {}", task.reference, task.title).set_style(style));
            let wrapped = adaptive_wrap_line(&task, opts);
            let mut out = Vec::new();
            push_owned_lines(&wrapped, &mut out);
            out
        };

        let mut lines: Vec<Line<'static>> = vec![];
        lines.push(
            vec![
                "• ".dim(),
                format!("Plan {}", self.update.reference).bold(),
                format!(
                    " · {} · r{}",
                    self.update.status.as_ref(),
                    self.update.revision
                )
                .dim(),
            ]
            .into(),
        );

        let mut indented_lines = vec![];
        let title = Line::from(self.update.title.clone().bold());
        push_owned_lines(
            &adaptive_wrap_line(
                &title,
                RtOptions::new(width.saturating_sub(4).max(1) as usize),
            ),
            &mut indented_lines,
        );

        if self.update.tasks.is_empty() {
            indented_lines.push(Line::from("(no tasks on this page)".dim().italic()));
        } else {
            for task in &self.update.tasks {
                indented_lines.extend(render_task(task));
            }
        }
        if let Some(offset) = self.update.next_offset {
            indented_lines.push(Line::from(
                format!("More tasks: read offset {offset}").dim(),
            ));
        }
        lines.extend(prefix_lines(indented_lines, "  └ ".dim(), "    ".into()));

        lines
    }
}

/// Render a bounded database snapshot with stable task references and nesting.
pub fn new_plan_update(update: PlanUpdate) -> PlanUpdateCell {
    PlanUpdateCell { update }
}
