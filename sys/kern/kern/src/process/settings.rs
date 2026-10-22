use crate::models_manager::catalog::ModelPickerCatalog;

use super::Process;

/// A mode permitted by this process's native mode policy.
#[derive(Clone, Debug)]
pub struct ProcessMode {
    pub id: String,
    pub title: String,
    pub description: String,
}

/// Public controls, without account secrets or unfiltered installation settings.
#[derive(Clone, Debug)]
pub struct ProcessSessionControls {
    /// Account-qualified selection id; provider requests use the raw model slug.
    pub model: String,
    pub current_mode: String,
    pub modes: Vec<ProcessMode>,
    pub switching_allowed: bool,
    pub model_catalog: ModelPickerCatalog,
}

/// A rejected control has no effect; an unconfirmed control must not be replayed.
#[derive(Debug, thiserror::Error)]
pub enum ProcessControlError {
    #[error("{0}")]
    Rejected(String),
    #[error("native session configuration durability is unconfirmed")]
    Unconfirmed(#[source] std::io::Error),
}

impl Process {
    /// Enabled user prompts from this process's runtime database, never editor
    /// supplied files or configuration.
    pub async fn custom_prompts(&self) -> Vec<chaos_ipc::custom_prompts::CustomPrompt> {
        let config = self.chaos.session.effective_config_for_spawn().await;
        crate::custom_prompts::discover_prompts(&config.chaos_home).await
    }

    /// Read native mode policy and cached choices from every usable account.
    pub async fn session_controls(&self) -> ProcessSessionControls {
        self.chaos.session.session_controls().await
    }

    /// Select a cached model/account pair without changing permissions. Legacy
    /// bare model slugs refer only to the currently selected account.
    /// In-flight model requests finish normally; subsequent samples use the change.
    pub async fn set_session_model(&self, model: &str) -> Result<(), ProcessControlError> {
        self.chaos
            .session
            .set_session_controls(Some(model), None, false)
            .await
    }

    /// Switch through the same native authority as `switch_mode`, including mid-turn.
    pub async fn set_session_mode(&self, mode: &str) -> Result<(), ProcessControlError> {
        self.chaos
            .session
            .set_session_controls(None, Some(mode), false)
            .await
    }

    /// Restore only identifiers under current native policy, never old grants.
    pub async fn restore_session_controls(
        &self,
        model: &str,
        mode: &str,
    ) -> Result<(), ProcessControlError> {
        self.chaos
            .session
            .set_session_controls(Some(model), Some(mode), true)
            .await
    }

    /// Confirm metadata and current ACP controls in the native journal.
    pub async fn confirm_session_durable(&self) -> std::io::Result<()> {
        self.chaos.session.confirm_session_controls().await
    }
}
