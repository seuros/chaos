use crate::{ProcessControlError, ProcessMode, ProcessSessionControls};
use chaos_ipc::protocol::{EventMsg, RolloutItem, SessionModeChangedEvent, SessionSource};

use super::Session;

impl Session {
    pub(crate) async fn session_controls(&self) -> ProcessSessionControls {
        let config = self.state.lock().await.session_configuration.clone();
        let model_catalog = self
            .services
            .models_manager
            .model_picker_catalog(
                &config.original_config_do_not_use.model_providers,
                &config.original_config_do_not_use.model_provider_id,
                crate::models_manager::RefreshStrategy::Offline,
            )
            .await;
        ProcessSessionControls {
            model: config.model_selection_id(),
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
            model_catalog,
        }
    }

    pub(crate) async fn set_session_controls(
        &self,
        model: Option<&str>,
        mode: Option<&str>,
        idle_only: bool,
    ) -> Result<(), ProcessControlError> {
        // Keep the selected turn alive until its history/checkpoint updates are
        // confirmed, and exclude a new turn from racing an idle control change.
        let active = self.active_turn.lock().await;
        if idle_only && active.is_some() {
            return Err(ProcessControlError::Rejected(
                "native session is busy".into(),
            ));
        }
        let controls = self.session_controls().await;
        let requested_model = model.unwrap_or(&controls.model);
        let current = crate::models_manager::catalog::ModelSelection::from_id(&controls.model)
            .expect("native model identity must be qualified");
        let model = if crate::models_manager::catalog::ModelSelection::same_model(
            requested_model,
            &controls.model,
            &current.provider_id,
        ) {
            controls.model.as_str()
        } else {
            requested_model
        };
        let mode = mode.unwrap_or(&controls.current_mode);
        let choice = controls.model_catalog.find(model, &current.provider_id);
        if model != controls.model && choice.is_none() {
            return Err(ProcessControlError::Rejected(
                "model/account choice is not available".into(),
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
        if model != controls.model {
            let mut next = state.session_configuration.clone();
            next.collaboration_mode = next.collaboration_mode.with_updates(
                Some(choice.expect("validated model choice").id.clone()),
                None,
                None,
            );
            self.resolve_model_configuration(&mut next)
                .await
                .map_err(|error| ProcessControlError::Rejected(error.to_string()))?;
            state.session_configuration = next;
        }
        drop(state);
        if !mode_changed {
            return self
                .confirm_session_controls()
                .await
                .map_err(ProcessControlError::Unconfirmed);
        }
        let current = active.as_ref().and_then(|turn| {
            turn.tasks
                .values()
                .next()
                .map(|task| task.turn_context.clone())
        });
        let temporary = current.is_none();
        let turn = match current {
            Some(turn) => turn,
            None => self.new_default_turn().await,
        };
        let result = self
            .switch_mode(mode, &turn)
            .await
            .map(|_| ())
            .map_err(|error| ProcessControlError::Unconfirmed(std::io::Error::other(error)));
        if temporary {
            turn.turn_metadata_state.cancel_git_enrichment_task();
            self.permission_actor
                .remove_turn(turn.sub_id.clone())
                .await
                .map_err(|_| {
                    ProcessControlError::Unconfirmed(std::io::Error::other(
                        "native mode control cleanup is unconfirmed",
                    ))
                })?;
        }
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
                model: config.model_selection_id(),
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
