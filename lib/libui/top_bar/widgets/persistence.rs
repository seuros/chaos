//! Highest-priority warning for an unhealthy session journal.

use chaos_kern::{PersistenceHealth, PersistenceStatus};
use tokio::sync::watch;

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn new(source: watch::Receiver<PersistenceStatus>) -> BarWidget {
    BarWidget::watched(
        "persistence",
        Side::Right,
        255,
        source,
        |status| status.health,
        present,
    )
}

fn present(health: PersistenceHealth) -> Content {
    let tone = match health {
        PersistenceHealth::Healthy => return Content::default(),
        PersistenceHealth::Degraded => Tone::Warning,
        PersistenceHealth::Failing | PersistenceHealth::Failed => Tone::Error,
    };
    Content::new("⚠ log").tone(tone)
}
