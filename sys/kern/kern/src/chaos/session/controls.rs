use crate::{ProcessControlError, ProcessMode, ProcessSessionControls};
use chaos_ipc::protocol::{EventMsg, RolloutItem, SessionModeChangedEvent, SessionSource};

use super::Session;

impl Session {
    pub(crate) async fn session_controls(&self) -> ProcessSessionControls {
        let state = self.state.lock().await;
        let config = &state.session_configuration;
        ProcessSessionControls {
            model: config.collaboration_mode.model().to_string(),
            current_mode: config.mode_policy.active_mode.clone(),
            modes: config
                .mode_policy
                .allowed_modes
                .iter()
                .filter_map(|id| config.mode_registry.get(id))
                .map(|mode| ProcessMode {
                    id: mode.id.clone(),
                    title: mode.title.clone(),
                    description: mode.description.clone(),
                })
                .collect(),
            switching_allowed: config.mode_policy.switching_allowed,
            models: self
                .services
                .models_manager
                .try_list_models()
                .unwrap_or_default()
                .into_iter()
                .filter(|model| model.show_in_picker && model.supported_in_api)
                .collect(),
        }
    }

    pub(crate) async fn set_idle_controls(
        &self,
        model: Option<&str>,
        mode: Option<&str>,
    ) -> Result<(), ProcessControlError> {
        let idle = self.active_turn.lock().await;
        if idle.is_some() {
            return Err(ProcessControlError::Rejected(
                "native session is busy".into(),
            ));
        }
        let controls = self.session_controls().await;
        let model = model.unwrap_or(&controls.model);
        let mode = mode.unwrap_or(&controls.current_mode);
        if model != controls.model && !controls.models.iter().any(|entry| entry.model == model) {
            return Err(ProcessControlError::Rejected(
                "model is not available in the selected provider catalog".into(),
            ));
        }
        let mode_changed = mode != controls.current_mode;
        if !controls.modes.iter().any(|entry| entry.id == mode)
            || (mode_changed && !controls.switching_allowed)
        {
            return Err(ProcessControlError::Rejected(
                "mode is not allowed for this session".into(),
            ));
        }
        let mut state = self.state.lock().await;
        let config = &mut state.session_configuration;
        config.collaboration_mode =
            config
                .collaboration_mode
                .with_updates(Some(model.to_string()), None, None);
        drop(state);
        if !mode_changed {
            return self
                .confirm_session_controls()
                .await
                .map_err(ProcessControlError::Unconfirmed);
        }
        let turn = self.new_default_turn().await;
        let result = self
            .switch_mode(mode, &turn)
            .await
            .map(|_| ())
            .map_err(|error| ProcessControlError::Unconfirmed(std::io::Error::other(error)));
        turn.turn_metadata_state.cancel_git_enrichment_task();
        self.permission_actor
            .remove_turn(turn.sub_id.clone())
            .await
            .map_err(|_| {
                ProcessControlError::Unconfirmed(std::io::Error::other(
                    "native mode control cleanup is unconfirmed",
                ))
            })?;
        result
    }

    pub(crate) async fn confirm_session_controls(&self) -> std::io::Result<()> {
        let event = {
            let state = self.state.lock().await;
            let config = &state.session_configuration;
            if config.session_source != SessionSource::Acp {
                return Ok(());
            }
            let mode = config
                .mode_registry
                .get(&config.mode_policy.active_mode)
                .ok_or_else(|| std::io::Error::other("native mode is unavailable"))?;
            SessionModeChangedEvent {
                session_id: self.conversation_id,
                mode_id: mode.id.clone(),
                mode_title: mode.title.clone(),
                mode_kind: config.collaboration_mode.mode,
                model: config.collaboration_mode.model().to_string(),
                reasoning_effort: config.collaboration_mode.reasoning_effort(),
            }
        };
        let recorder = self
            .services
            .rollout
            .lock()
            .await
            .clone()
            .ok_or_else(|| std::io::Error::other("native journal is unavailable"))?;
        recorder
            .record_items(&[RolloutItem::EventMsg(EventMsg::SessionModeChanged(event))])
            .await?;
        recorder.confirm().await
    }
}
