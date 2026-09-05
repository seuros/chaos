//! Runtime-storage backend, updated from kernel status notifications.

use chaos_kern::{PersistenceStatus, RuntimeStorageBackend};
use tokio::sync::watch;

use super::super::{BarWidget, Content, Side, Tone};

pub(in crate::top_bar) fn new(source: watch::Receiver<PersistenceStatus>) -> BarWidget {
    BarWidget::watched(
        "storage",
        Side::Right,
        200,
        source,
        |status| status.backend,
        present,
    )
}

fn present(backend: RuntimeStorageBackend) -> Content {
    match backend {
        RuntimeStorageBackend::Sqlite => Content::new("SQLITE"),
        RuntimeStorageBackend::Postgres => Content::new("🐘").tone(Tone::Accent),
    }
}
