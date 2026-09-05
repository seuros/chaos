//! Optional container or jail identity.

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn new(present: bool, kind: &str) -> BarWidget {
    let label = if !present {
        ""
    } else if kind.is_empty() {
        "container"
    } else {
        kind
    };
    BarWidget::text(
        "container",
        Side::Left,
        150,
        Content::new(label.to_owned()).tone(Tone::Warning),
    )
}
