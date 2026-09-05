//! Static machine identity, measured without truncating the hostname.

use super::super::{BarWidget, Content, Side};

pub(in crate::top_bar) fn new(name: String) -> BarWidget {
    BarWidget::text("hostname", Side::Left, 180, Content::new(name).bold())
}
