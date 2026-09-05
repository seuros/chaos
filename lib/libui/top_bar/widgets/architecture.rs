//! Static CPU architecture.

use super::super::{BarWidget, Content, Side};

pub(in crate::top_bar) fn new(label: String) -> BarWidget {
    BarWidget::text("architecture", Side::Left, 80, Content::new(label))
}
