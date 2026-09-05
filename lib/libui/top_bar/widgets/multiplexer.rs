//! Multiplexer identity uses stable pane/session IDs, never renumbered coordinates.

use chaos_sysinfo::MultiplexerInfo;

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn new(info: Option<&MultiplexerInfo>) -> BarWidget {
    let label = info.map_or_else(String::new, |info| {
        if info.id.is_empty() {
            info.kind.clone()
        } else {
            format!("{} {}", info.kind, info.id)
        }
    });
    BarWidget::text(
        "multiplexer",
        Side::Left,
        120,
        Content::new(label).tone(Tone::Accent),
    )
}
