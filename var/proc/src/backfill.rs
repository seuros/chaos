//! Backfill API surface for runtime state.

use crate::BackfillState;
use crate::runtime::StateRuntime;

#[derive(Clone, Copy)]
pub struct Backfill<'a> {
    runtime: &'a StateRuntime,
}

impl StateRuntime {
    pub fn backfill(&self) -> Backfill<'_> {
        Backfill { runtime: self }
    }
}

impl<'a> Backfill<'a> {
    pub async fn get_state(&self) -> anyhow::Result<BackfillState> {
        self.runtime.get_backfill_state().await
    }

    pub async fn try_claim(&self, lease_seconds: i64) -> anyhow::Result<bool> {
        self.runtime.try_claim_backfill(lease_seconds).await
    }

    pub async fn mark_running(&self) -> anyhow::Result<()> {
        self.runtime.mark_backfill_running().await
    }

    pub async fn checkpoint(&self, watermark: &str) -> anyhow::Result<()> {
        self.runtime.checkpoint_backfill(watermark).await
    }

    pub async fn mark_complete(&self, last_watermark: Option<&str>) -> anyhow::Result<()> {
        self.runtime.mark_backfill_complete(last_watermark).await
    }
}
