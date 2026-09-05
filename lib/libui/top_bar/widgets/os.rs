//! OS and distribution identity, collected once.

use super::super::{BarWidget, Content, Side};

pub(in crate::top_bar) fn new(os: &str, distro: &str) -> BarWidget {
    let label = if distro.is_empty() {
        os.to_owned()
    } else {
        format!("{os} ({distro})")
    };
    BarWidget::text("os", Side::Left, 100, Content::new(label))
}
