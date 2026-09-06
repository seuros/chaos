//! Recovery discovery and policy validation for userland supervisors.
//!
//! The existing rebuildable process index supplies discovery; the append-only
//! journal, not the index, decides eligibility. Both mounted backends use this
//! same projection.

use crate::background_tasks::TaskRegistry;
use crate::config::{Config, ConfigBuilder, ConfigOverrides};
use crate::rollout::{
    RolloutRecorder,
    list::{Cursor, ProcessSortKey},
};
use anyhow::bail;
use chaos_ipc::ProcessId;
use chaos_ipc::background_tasks::{TaskJournalEvent, TaskRecoveryContext, WakePolicy};
use chaos_ipc::protocol::{ApprovalPolicy, RolloutItem, SessionSource, TurnContextItem};
use std::path::PathBuf;

pub struct RecoveryCandidate {
    pub process_id: ProcessId,
    context: TurnContextItem,
    recovery: TaskRecoveryContext,
}

impl RecoveryCandidate {
    /// Reload current configuration and constraints. A saved grant is never
    /// applied to the new owner. Policy changes requiring reconciliation fail
    /// closed instead of silently broadening access.
    pub async fn load_config(
        &self,
        alcatraz_exe: PathBuf,
        overrides: Vec<(String, toml::Value)>,
    ) -> anyhow::Result<Config> {
        let mut config = ConfigBuilder::default()
            .cli_overrides(overrides)
            .harness_overrides(ConfigOverrides {
                cwd: Some(self.context.cwd.clone()),
                model: Some(self.context.model.clone()),
                model_provider: Some(self.recovery.provider.clone()),
                approval_policy: Some(ApprovalPolicy::Headless),
                alcatraz_exe: Some(alcatraz_exe),
                ..Default::default()
            })
            .build()
            .await?;
        if config.ephemeral {
            bail!("ephemeral configuration cannot recover a durable process");
        }
        if config.permissions.vfs_policy != self.context.vfs_policy
            || config.permissions.socket_policy != self.context.socket_policy
        {
            bail!(
                "saved permissions differ from current constraints; owner reconciliation required"
            );
        }
        let registry = crate::modes::ModeRegistry::load(
            &config.chaos_home,
            crate::collaboration_modes::CollaborationModesConfig::default(),
        )?;
        let policy = crate::modes::ModePolicy {
            active_mode: self.recovery.mode_id.clone(),
            allowed_modes: self.recovery.allowed_modes.clone(),
            switching_allowed: self.recovery.switching_allowed,
        };
        policy.validate(&registry).map_err(anyhow::Error::msg)?;
        config.mode_policy_override = Some(policy);
        config.unattended_recovery = true;
        config.model_reasoning_effort = self.context.effort;
        config.model_reasoning_summary = Some(self.context.summary);
        crate::auth::enforce_login_restrictions(&config)?;
        Ok(config)
    }
}

/// Recheck the snapshot after writer acquisition, closing the discovery/claim
/// race without treating saved policy as a grant.
pub(crate) async fn validate_claimed(config: &Config, items: &[RolloutItem]) -> anyhow::Result<()> {
    let context = items
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::TurnContext(context) => Some(context),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("missing recovery turn context"))?;
    let recovery = items
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::BackgroundTask(TaskJournalEvent::RecoveryContext { context }) => {
                Some(context)
            }
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("missing recovery policy"))?;
    let mode = config
        .mode_policy_override
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing recovery mode"))?;
    if config.cwd != context.cwd
        || config.model.as_deref() != Some(&context.model)
        || config.model_provider_id != recovery.provider
        || config.permissions.vfs_policy != context.vfs_policy
        || config.permissions.socket_policy != context.socket_policy
        || mode.active_mode != recovery.mode_id
        || mode.allowed_modes != recovery.allowed_modes
        || mode.switching_allowed != recovery.switching_allowed
    {
        bail!("recovery context changed before ownership was acquired");
    }
    let registry = TaskRegistry::default();
    registry.restore(items).await;
    if registry.subscribe().borrow().wake_policy != WakePolicy::Enabled {
        bail!("process no longer permits automatic background recovery");
    }
    Ok(())
}

/// One bounded page. A daemon follows the cursor and rescans after reaching the
/// end; pre-upgrade, archived, closed, interrupted, and child sessions are not
/// automatically enrolled.
pub async fn discover(
    config: &Config,
    cursor: Option<&Cursor>,
) -> anyhow::Result<(Vec<RecoveryCandidate>, Option<Cursor>)> {
    let page = RolloutRecorder::list_processes(
        config,
        100,
        cursor,
        ProcessSortKey::UpdatedAt,
        &[],
        &config.model_provider_id,
        None,
    )
    .await?;
    let mut candidates = Vec::new();
    for process in page.items {
        let Some(process_id) = process.process_id else {
            continue;
        };
        if matches!(process.source, Some(SessionSource::SubAgent(_))) {
            continue;
        }
        let history = match RolloutRecorder::get_rollout_history_for_process(process_id).await {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(%process_id, %error, "cannot inspect recovery candidate");
                continue;
            }
        };
        let items = history.get_rollout_items();
        let tasks = TaskRegistry::default();
        tasks.restore(&items).await;
        let activity = tasks.subscribe().borrow().clone();
        if activity.wake_policy != WakePolicy::Enabled || activity.is_quiescent() {
            continue;
        }
        let context = items.iter().rev().find_map(|item| match item {
            RolloutItem::TurnContext(context) => Some(context.clone()),
            _ => None,
        });
        let recovery = items.iter().rev().find_map(|item| match item {
            RolloutItem::BackgroundTask(TaskJournalEvent::RecoveryContext { context }) => {
                Some(context.clone())
            }
            _ => None,
        });
        // Dynamic client tools have no unattended implementation.
        let has_dynamic_tools = items.iter().any(|item| matches!(item,
            RolloutItem::SessionMeta(meta) if meta.meta.dynamic_tools.as_ref().is_some_and(|tools| !tools.is_empty())));
        if has_dynamic_tools {
            continue;
        }
        if let (Some(context), Some(recovery)) = (context, recovery) {
            candidates.push(RecoveryCandidate {
                process_id,
                context,
                recovery,
            });
        }
    }
    Ok((candidates, page.next_cursor))
}
