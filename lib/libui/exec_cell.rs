mod model;
mod render;

pub use model::CommandOutput;
#[cfg(test)]
pub use model::ExecCall;
pub use model::ExecCell;
pub use render::OutputLinesParams;
pub use render::TOOL_CALL_MAX_LINES;
pub use render::new_active_exec_command;
pub use render::output_lines;
pub use render::spinner;

#[cfg(test)]
pub(crate) mod tests {
    pub(crate) fn exec_cell_suite() {
        super::render::tests::exec_cell_render_suite();
    }
}
