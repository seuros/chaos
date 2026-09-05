//! Local clock: cached HH:MM presentation, refreshed at minute boundaries.

use std::time::Duration;

use jiff::Zoned;

use super::super::{BarWidget, Content, Side};

pub(in crate::top_bar) fn new() -> BarWidget {
    BarWidget::timed("clock", Side::Right, 220, sample)
}

fn sample(now: &Zoned) -> (Content, Duration) {
    let until_minute = Duration::from_secs((60 - now.second()) as u64)
        - Duration::from_nanos(now.subsec_nanosecond() as u64);
    (
        Content::new(now.strftime("%H:%M").to_string()),
        until_minute,
    )
}
