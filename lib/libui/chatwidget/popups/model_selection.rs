//! Model selection popup methods.
use super::super::*;

impl ChatWidget {
    /// Open a popup to choose a quick auto model. Selecting "All models"
    /// opens the full picker with every available preset.
    pub fn open_model_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Model selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let presets: Vec<ModelPreset> = if crate::theme::is_clamped() {
            use chaos_ipc::openai_models::ReasoningEffort;
            use chaos_ipc::openai_models::ReasoningEffortPreset;

            // Build presets from the real Claude Code init response.
            let cached = chaos_clamp::cached_models();
            if let Some(models_json) = cached.as_ref().and_then(|v| v.as_array()) {
                models_json
                    .iter()
                    .map(|m| {
                        let value = m.get("value").and_then(|v| v.as_str()).unwrap_or("default");
                        let display = m
                            .get("displayName")
                            .and_then(|v| v.as_str())
                            .unwrap_or(value);
                        let desc = m.get("description").and_then(|v| v.as_str()).unwrap_or("");
                        let is_default = value == "default";
                        let efforts: Vec<ReasoningEffortPreset> = m
                            .get("supportedEffortLevels")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|e| {
                                        let s = e.as_str()?;
                                        let effort = match s {
                                            "low" => ReasoningEffort::Low,
                                            "medium" => ReasoningEffort::Medium,
                                            "high" => ReasoningEffort::High,
                                            _ => return None,
                                        };
                                        Some(ReasoningEffortPreset {
                                            effort,
                                            description: s.to_string(),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        ModelPreset {
                            id: value.to_string(),
                            model: value.to_string(),
                            display_name: display.to_string(),
                            description: desc.to_string(),
                            default_reasoning_effort: ReasoningEffort::Medium,
                            supported_reasoning_efforts: efforts,
                            supports_personality: false,
                            is_default,
                            show_in_picker: true,
                            availability_nux: None,
                            supported_in_api: true,
                            input_modalities: vec![],
                        }
                    })
                    .collect()
            } else {
                // No cached models yet — subprocess hasn't been spawned.
                self.add_info_message(
                    "Send a message first to initialize the Claude Code subprocess, \
                     then try /model again."
                        .to_string(),
                    None,
                );
                return;
            }
        } else {
            match self.models_manager.try_list_models() {
                Ok(models) => models,
                Err(_) => {
                    self.add_info_message(
                        "Models are being updated; please try /model again in a moment."
                            .to_string(),
                        /*hint*/ None,
                    );
                    return;
                }
            }
        };
        self.open_model_popup_with_presets(presets);
    }

    pub fn open_personality_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Personality selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }
        if !self.current_model_supports_personality() {
            let current_model = self.current_model();
            self.add_error_message(format!(
                "Current model ({current_model}) doesn't support personalities. Try /model to pick a different model."
            ));
            return;
        }
        self.open_personality_popup_for_current_model();
    }

    pub(crate) fn open_personality_popup_for_current_model(&mut self) {
        let current_personality = self.config.personality.unwrap_or(Personality::Friendly);
        let personalities = [Personality::Friendly, Personality::Pragmatic];
        let supports_personality = self.current_model_supports_personality();

        let items: Vec<SelectionItem> = personalities
            .into_iter()
            .map(|personality| {
                let name = Self::personality_label(personality).to_string();
                let description = Some(Self::personality_description(personality).to_string());
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::ChaosOp(Op::OverrideTurnContext {
                        cwd: None,
                        approval_policy: None,
                        approvals_reviewer: None,
                        sandbox_policy: None,
                        model: None,
                        effort: None,
                        summary: None,
                        service_tier: None,
                        collaboration_mode: None,

                        personality: Some(personality),
                    }));
                    tx.send(AppEvent::UpdatePersonality(personality));
                    tx.send(AppEvent::PersistPersonalitySelection { personality });
                })];
                SelectionItem {
                    name,
                    description,
                    is_current: current_personality == personality,
                    is_disabled: !supports_personality,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Personality".bold()));
        header.push(Line::from("Choose a communication style for Chaos.".dim()));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn model_menu_header(&self, title: &str, subtitle: &str) -> Box<dyn Renderable> {
        let title = title.to_string();
        let subtitle = subtitle.to_string();
        let mut header = ColumnRenderable::new();
        header.push(Line::from(title.bold()));
        header.push(Line::from(subtitle.dim()));
        if let Some(warning) = self.model_menu_warning_line() {
            header.push(warning);
        }
        Box::new(header)
    }

    pub(crate) fn model_menu_warning_line(&self) -> Option<Line<'static>> {
        let base_url = self.custom_openai_base_url()?;
        let warning = format!(
            "Warning: OpenAI base URL is overridden to {base_url}. Selecting models may not be supported or work properly."
        );
        Some(Line::from(warning.red()))
    }

    pub(crate) fn custom_openai_base_url(&self) -> Option<String> {
        if !self.config.model_provider.is_openai() {
            return None;
        }

        let base_url = self.config.model_provider.base_url.as_ref()?;
        let trimmed = base_url.trim();
        if trimmed.is_empty() {
            return None;
        }

        let normalized = trimmed.trim_end_matches('/');
        if normalized == OPENAI_DEFAULT_BASE_URL {
            return None;
        }

        Some(trimmed.to_string())
    }

    pub fn open_model_popup_with_presets(&mut self, presets: Vec<ModelPreset>) {
        let presets: Vec<ModelPreset> = presets
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect();

        let current_model = self.current_model();
        let current_label = presets
            .iter()
            .find(|preset| preset.model.as_str() == current_model)
            .map(|preset| preset.model.to_string())
            .unwrap_or_else(|| self.model_display_name().to_string());

        let (mut auto_presets, other_presets): (Vec<ModelPreset>, Vec<ModelPreset>) = presets
            .into_iter()
            .partition(|preset| Self::is_auto_model(&preset.model));

        if auto_presets.is_empty() {
            self.open_all_models_popup(other_presets);
            return;
        }

        auto_presets.sort_by_key(|preset| Self::auto_model_order(&preset.model));
        let mut items: Vec<SelectionItem> = auto_presets
            .into_iter()
            .map(|preset| {
                let description =
                    (!preset.description.is_empty()).then_some(preset.description.clone());
                let model = preset.model.clone();
                let should_prompt_plan_mode_scope = self.should_prompt_plan_mode_reasoning_scope(
                    model.as_str(),
                    Some(preset.default_reasoning_effort),
                );
                let actions = Self::model_selection_actions(
                    model.clone(),
                    Some(preset.default_reasoning_effort),
                    should_prompt_plan_mode_scope,
                );
                SelectionItem {
                    name: model.clone(),
                    description,
                    is_current: model.as_str() == current_model,
                    is_default: preset.is_default,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        if !other_presets.is_empty() {
            let all_models = other_presets;
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenAllModelsPopup {
                    models: all_models.clone(),
                });
            })];

            let is_current = !items.iter().any(|item| item.is_current);
            let description = Some(format!(
                "Choose a specific model and reasoning level (current: {current_label})"
            ));

            items.push(SelectionItem {
                name: "All models".to_string(),
                description,
                is_current,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model",
            "Pick a quick auto mode or browse all models.",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header,
            ..Default::default()
        });
    }

    pub(crate) fn is_auto_model(model: &str) -> bool {
        model.starts_with("chaos-auto-")
    }

    pub(crate) fn auto_model_order(model: &str) -> usize {
        match model {
            "chaos-auto-fast" => 0,
            "chaos-auto-balanced" => 1,
            "chaos-auto-thorough" => 2,
            _ => 3,
        }
    }

    pub fn open_all_models_popup(&mut self, presets: Vec<ModelPreset>) {
        if presets.is_empty() {
            self.add_info_message(
                "No additional models are available right now.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let mut items: Vec<SelectionItem> = Vec::new();
        for preset in presets.into_iter() {
            let description =
                (!preset.description.is_empty()).then_some(preset.description.to_string());
            let is_current = preset.model.as_str() == self.current_model();
            let single_supported_effort = preset.supported_reasoning_efforts.len() == 1;
            let preset_for_action = preset.clone();
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                let preset_for_event = preset_for_action.clone();
                tx.send(AppEvent::OpenReasoningPopup {
                    model: preset_for_event,
                });
            })];
            items.push(SelectionItem {
                name: preset.model.clone(),
                description,
                is_current,
                is_default: preset.is_default,
                actions,
                dismiss_on_select: single_supported_effort,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model and Effort",
            "Access legacy models by running chaos -m <model_name> or in your config.toml",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some("Press enter to select reasoning effort, or esc to dismiss.".into()),
            items,
            header,
            ..Default::default()
        });
    }

    pub fn open_collaboration_modes_popup(&mut self) {
        let presets = collaboration_modes::presets_for_tui(self.models_manager.as_ref());
        if presets.is_empty() {
            self.add_info_message(
                "No collaboration modes are available right now.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let current_kind = self
            .active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.mode)
            .or_else(|| {
                collaboration_modes::default_mask(self.models_manager.as_ref())
                    .and_then(|mask| mask.mode)
            });
        let items: Vec<SelectionItem> = presets
            .into_iter()
            .map(|mask| {
                let name = mask.name.clone();
                let is_current = current_kind == mask.mode;
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::UpdateCollaborationMode(mask.clone()));
                })];
                SelectionItem {
                    name,
                    is_current,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select Collaboration Mode".to_string()),
            subtitle: Some("Pick a collaboration preset.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn model_selection_actions(
        model_for_action: String,
        effort_for_action: Option<ReasoningEffortConfig>,
        should_prompt_plan_mode_scope: bool,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            if should_prompt_plan_mode_scope {
                tx.send(AppEvent::OpenPlanReasoningScopePrompt {
                    model: model_for_action.clone(),
                    effort: effort_for_action,
                });
                return;
            }

            tx.send(AppEvent::UpdateModel(model_for_action.clone()));
            tx.send(AppEvent::UpdateReasoningEffort(effort_for_action));
            Self::queue_persist_model_selection(
                tx,
                model_for_action.clone(),
                effort_for_action,
                crate::theme::is_clamped(),
            );
        })]
    }

    pub(crate) fn apply_model_and_effort_without_persist(
        &self,
        model: String,
        effort: Option<ReasoningEffortConfig>,
    ) {
        self.app_event_tx.send(AppEvent::UpdateModel(model));
        self.app_event_tx
            .send(AppEvent::UpdateReasoningEffort(effort));
    }

    pub(crate) fn queue_persist_model_selection(
        tx: &AppEventSender,
        model: String,
        effort: Option<ReasoningEffortConfig>,
        clamped: bool,
    ) {
        if clamped {
            return;
        }

        tx.send(AppEvent::PersistModelSelection { model, effort });
    }

    pub(crate) fn apply_model_and_effort(
        &self,
        model: String,
        effort: Option<ReasoningEffortConfig>,
    ) {
        self.apply_model_and_effort_without_persist(model.clone(), effort);
        Self::queue_persist_model_selection(
            &self.app_event_tx,
            model,
            effort,
            crate::theme::is_clamped(),
        );
    }

    pub(crate) fn capture_pre_clamp_selection(&mut self) {
        self.pre_clamp_selection = Some(PreClampSelection {
            model: self.current_model().to_string(),
            reasoning_effort: self.current_collaboration_mode.reasoning_effort(),
            plan_mode_reasoning_effort: self.config.plan_mode_reasoning_effort,
        });
    }

    pub(crate) fn restore_pre_clamp_selection(&mut self) {
        let Some(previous) = self.pre_clamp_selection.take() else {
            return;
        };

        self.app_event_tx
            .send(AppEvent::UpdateModel(previous.model));
        self.app_event_tx
            .send(AppEvent::UpdateReasoningEffort(previous.reasoning_effort));
        self.app_event_tx
            .send(AppEvent::UpdatePlanModeReasoningEffort(
                previous.plan_mode_reasoning_effort,
            ));
    }
}
