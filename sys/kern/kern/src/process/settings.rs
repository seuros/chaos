use chaos_ipc::openai_models::ModelPreset;

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
    pub model: String,
    pub current_mode: String,
    pub modes: Vec<ProcessMode>,
    pub switching_allowed: bool,
    pub models: Vec<ModelPreset>,
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

    /// Read the session's native mode policy and the selected provider's cached catalog.
    pub async fn session_controls(&self) -> ProcessSessionControls {
        self.chaos.session.session_controls().await
    }

    /// Change only the model of an idle session, never its provider or permissions.
    pub async fn set_session_model(&self, model: &str) -> Result<(), ProcessControlError> {
        self.chaos
            .session
            .set_idle_controls(Some(model), None)
            .await
    }

    /// Switch an idle session through the same native authority as `switch_mode`.
    pub async fn set_session_mode(&self, mode: &str) -> Result<(), ProcessControlError> {
        self.chaos.session.set_idle_controls(None, Some(mode)).await
    }

    /// Restore only identifiers under current native policy, never old grants.
    pub async fn restore_session_controls(
        &self,
        model: &str,
        mode: &str,
    ) -> Result<(), ProcessControlError> {
        self.chaos
            .session
            .set_idle_controls(Some(model), Some(mode))
            .await
    }

    /// Confirm metadata and current ACP controls in the native journal.
    pub async fn confirm_session_durable(&self) -> std::io::Result<()> {
        self.chaos.session.confirm_session_controls().await
    }
}
