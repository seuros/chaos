use crate::chaos::Session;
use crate::config::Config;
use crate::features::Feature;
use crate::memories::consolidation;
use crate::memories::extraction;
use chaos_ipc::protocol::SessionSource;
use chaos_traits::MementoConfig;
use std::sync::Arc;
use tracing::warn;

/// Starts the asynchronous startup memory pipeline for an eligible root session.
///
/// The pipeline is skipped for ephemeral sessions, disabled feature flags, and
/// subagent sessions.
pub(crate) fn start_memories_startup_task(
    session: &Arc<Session>,
    config: Arc<Config>,
    source: &SessionSource,
) {
    if config.ephemeral()
        || !config.features().enabled(Feature::MemoryTool)
        || matches!(source, SessionSource::SubAgent(_))
    {
        return;
    }

    if session.state_db().is_none() {
        warn!("state db unavailable for memories startup pipeline; skipping");
        return;
    }

    let weak_session = Arc::downgrade(session);
    tokio::spawn(async move {
        let Some(session) = weak_session.upgrade() else {
            return;
        };

        // Prune stale raw memories to keep DB size bounded.
        extraction::prune(&session, &config).await;
        // Extract raw memories from eligible rollouts.
        extraction::run(&session, &config).await;
        // Consolidate raw memories via sub-agent.
        consolidation::run(&session, config).await;
    });
}
