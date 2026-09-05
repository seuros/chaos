//! Static product identity, with optional source revision for debug builds.

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn new() -> BarWidget {
    BarWidget::text(
        "version",
        Side::Left,
        170,
        present(cfg!(debug_assertions), option_env!("CHAOS_BUILD_SHA")),
    )
}

pub(in crate::top_bar) fn present(debug: bool, sha: Option<&str>) -> Content {
    let mut label = chaos_ipc::product::display_name_with_version();
    if debug {
        if let Some(sha) = sha.filter(|sha| !sha.is_empty()) {
            label.push(' ');
            label.extend(sha.chars().take(7));
        }
    }
    Content::new(label).tone(if debug { Tone::Warning } else { Tone::Normal })
}
