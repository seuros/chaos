//! Multiplexer identity uses stable pane/session IDs, never renumbered coordinates.

use chaos_sysinfo::terminal::Multiplexer;

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn new(info: Option<&Multiplexer>) -> BarWidget {
    let label = info.map_or_else(String::new, |mux| match mux.id() {
        Some(id) => format!("{} {id}", mux.kind()),
        None => mux.kind().to_string(),
    });
    BarWidget::text(
        "multiplexer",
        Side::Left,
        120,
        Content::new(label).tone(Tone::Accent),
    )
}
