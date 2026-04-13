//! Diff-related history cells.
//!
//! Covers cells that represent file-change patches: the inline patch summary
//! shown after an apply, the failure notice when a patch cannot be applied, and
//! the image-view record produced by the view_image tool call.

use crate::diff_render::create_diff_summary;
use crate::diff_render::display_path_for;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::OutputLinesParams;
use crate::exec_cell::TOOL_CALL_MAX_LINES;
use crate::exec_cell::output_lines;
use chaos_ipc::protocol::FileChange;
use ratatui::prelude::*;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use super::state::HistoryCell;
use super::state::PlainHistoryCell;

/// A history cell that renders a diff summary for a set of file changes.
#[derive(Debug)]
pub struct PatchHistoryCell {
    pub(super) changes: HashMap<PathBuf, FileChange>,
    pub(super) cwd: PathBuf,
}

impl HistoryCell for PatchHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        create_diff_summary(&self.changes, &self.cwd, width as usize)
    }
}

/// Create a `PatchHistoryCell` that lists the file-level summary of a proposed
/// patch. The summary lines should already be formatted (e.g. "A path/to/file.rs").
pub fn new_patch_event(changes: HashMap<PathBuf, FileChange>, cwd: &Path) -> PatchHistoryCell {
    PatchHistoryCell {
        changes,
        cwd: cwd.to_path_buf(),
    }
}

pub fn new_patch_apply_failure(stderr: String) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from("✘ Failed to apply patch".magenta().bold()));

    if !stderr.trim().is_empty() {
        let output = output_lines(
            Some(&CommandOutput {
                exit_code: 1,
                formatted_output: String::new(),
                aggregated_output: stderr,
            }),
            OutputLinesParams {
                line_limit: TOOL_CALL_MAX_LINES,
                only_err: true,
                include_angle_pipe: true,
                include_prefix: true,
            },
        );
        lines.extend(output.lines);
    }

    PlainHistoryCell::new(lines)
}

pub fn new_view_image_tool_call(path: PathBuf, cwd: &Path) -> PlainHistoryCell {
    let display_path = display_path_for(&path, cwd);

    let lines: Vec<Line<'static>> = vec![
        vec!["• ".dim(), "Viewed Image".bold()].into(),
        vec!["  └ ".dim(), display_path.dim()].into(),
    ];

    PlainHistoryCell::new(lines)
}
