//! Approval, sandbox, model, mode, theme, reasoning effort, and personality
//! configuration setters for `ChatWidget`.
use super::super::*;

impl ChatWidget {
    /// Set the approval policy in the widget's config copy.
    pub fn set_approval_policy(&mut self, policy: ApprovalPolicy) {
        if let Err(err) = self.config.permissions.approval_policy.set(policy) {
            tracing::warn!(%err, "failed to set approval_policy on chat config");
        }
    }

    /// Set the sandbox policy in the widget's config copy.
    pub fn set_sandbox_policy(&mut self, policy: SandboxPolicy) -> ConstraintResult<()> {
        self.config.permissions.sandbox_policy.set(policy)?;
        Ok(())
    }

    pub fn set_feature_enabled(&mut self, feature: Feature, enabled: bool) -> bool {
        if let Err(err) = self.config.features.set_enabled(feature, enabled) {
            tracing::warn!(
                error = %err,
                feature = feature.key(),
                "failed to update constrained chat widget feature state"
            );
        }
        self.config.features.enabled(feature)
    }

    pub fn set_approvals_reviewer(&mut self, policy: ApprovalsReviewer) {
        self.config.approvals_reviewer = policy;
    }

    pub fn set_full_access_warning_acknowledged(&mut self, acknowledged: bool) {
        self.config.notices.hide_full_access_warning = Some(acknowledged);
    }

    pub fn set_rate_limit_switch_prompt_hidden(&mut self, hidden: bool) {
        self.config.notices.hide_rate_limit_model_nudge = Some(hidden);
        if hidden {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
        }
    }

    pub fn set_plan_mode_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.config.plan_mode_reasoning_effort = effort;
        if self.collaboration_modes_enabled()
            && let Some(mask) = self.active_collaboration_mask.as_mut()
            && mask.mode == Some(ModeKind::Plan)
        {
            if let Some(effort) = effort {
                mask.reasoning_effort = Some(Some(effort));
            } else if let Some(plan_mask) =
                collaboration_modes::plan_mask(self.models_manager.as_ref())
            {
                mask.reasoning_effort = plan_mask.reasoning_effort;
            }
        }
        self.refresh_status_line();
    }

    /// Set the reasoning effort in the stored collaboration mode.
    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.current_collaboration_mode = self.current_collaboration_mode.with_updates(
            /*model*/ None,
            Some(effort),
            /*minion_instructions*/ None,
        );
        if self.collaboration_modes_enabled()
            && let Some(mask) = self.active_collaboration_mask.as_mut()
            && mask.mode != Some(ModeKind::Plan)
        {
            // Generic "global default" updates should not mutate the active Plan mask.
            // Plan reasoning is controlled by the Plan preset and Plan-only override updates.
            mask.reasoning_effort = Some(effort);
        }
        self.refresh_status_line();
    }

    /// Set the personality in the widget's config copy.
    pub fn set_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
    }

    /// Set the syntax theme override in the widget's config copy.
    pub fn set_tui_theme(&mut self, theme: Option<String>) {
        self.config.tui_theme = theme;
    }

    /// Set the model in the widget's config copy and stored collaboration mode.
    pub fn set_model(&mut self, model: &str) {
        self.current_collaboration_mode = self.current_collaboration_mode.with_updates(
            Some(model.to_string()),
            /*effort*/ None,
            /*minion_instructions*/ None,
        );
        if self.collaboration_modes_enabled()
            && let Some(mask) = self.active_collaboration_mask.as_mut()
        {
            mask.model = Some(model.to_string());
        }
        self.refresh_model_display();
        self.refresh_status_line();
    }

    pub fn current_model(&self) -> &str {
        if !self.collaboration_modes_enabled() {
            return self.current_collaboration_mode.model();
        }
        self.active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.model.as_deref())
            .unwrap_or_else(|| self.current_collaboration_mode.model())
    }

    pub(crate) fn sync_personality_command_enabled(&mut self) {
        self.bottom_pane.set_personality_command_enabled(true);
    }

    pub(crate) fn current_model_supports_personality(&self) -> bool {
        let model = self.current_model();
        self.models_manager
            .try_list_models()
            .ok()
            .and_then(|models| {
                models
                    .into_iter()
                    .find(|preset| preset.model == model)
                    .map(|preset| preset.supports_personality)
            })
            .unwrap_or(false)
    }

    /// Return whether the effective model currently advertises image-input support.
    ///
    /// We intentionally default to `true` when model metadata cannot be read so transient catalog
    /// failures do not hard-block user input in the UI.
    pub(crate) fn current_model_supports_images(&self) -> bool {
        let model = self.current_model();
        self.models_manager
            .try_list_models()
            .ok()
            .and_then(|models| {
                models
                    .into_iter()
                    .find(|preset| preset.model == model)
                    .map(|preset| preset.input_modalities.contains(&InputModality::Image))
            })
            .unwrap_or(true)
    }

    pub(crate) fn sync_image_paste_enabled(&mut self) {
        let enabled = self.current_model_supports_images();
        self.bottom_pane.set_image_paste_enabled(enabled);
    }

    pub(crate) fn image_inputs_not_supported_message(&self) -> String {
        format!(
            "Model {} does not support image inputs. Remove images or switch models.",
            self.current_model()
        )
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn current_collaboration_mode(&self) -> &CollaborationMode {
        &self.current_collaboration_mode
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn active_collaboration_mode_kind(&self) -> ModeKind {
        self.active_mode_kind()
    }

    pub(crate) fn is_session_configured(&self) -> bool {
        self.process_id.is_some()
    }

    pub(crate) fn collaboration_modes_enabled(&self) -> bool {
        true
    }

    pub(crate) fn initial_collaboration_mask(
        _config: &Config,
        models_manager: &ModelsManager,
        model_override: Option<&str>,
    ) -> Option<CollaborationModeMask> {
        let mut mask = collaboration_modes::default_mask(models_manager)?;
        if let Some(model_override) = model_override {
            mask.model = Some(model_override.to_string());
        }
        Some(mask)
    }

    pub(crate) fn active_mode_kind(&self) -> ModeKind {
        self.active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.mode)
            .unwrap_or(ModeKind::Default)
    }

    pub fn collaboration_mode_kind(&self) -> ModeKind {
        self.active_mode_kind()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn current_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        self.effective_reasoning_effort()
    }

    pub(crate) fn effective_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        if !self.collaboration_modes_enabled() {
            return self.current_collaboration_mode.reasoning_effort();
        }
        let current_effort = self.current_collaboration_mode.reasoning_effort();
        self.active_collaboration_mask
            .as_ref()
            .and_then(|mask| mask.reasoning_effort)
            .unwrap_or(current_effort)
    }

    pub(crate) fn effective_collaboration_mode(&self) -> CollaborationMode {
        if !self.collaboration_modes_enabled() {
            return self.current_collaboration_mode.clone();
        }
        self.active_collaboration_mask.as_ref().map_or_else(
            || self.current_collaboration_mode.clone(),
            |mask| self.current_collaboration_mode.apply_mask(mask),
        )
    }

    pub(crate) fn refresh_model_display(&mut self) {
        let effective = self.effective_collaboration_mode();
        self.session_header.set_model(effective.model());
        // Keep composer paste affordances aligned with the currently effective model.
        self.sync_image_paste_enabled();
        self.update_collaboration_mode_indicator();
    }

    pub(crate) fn model_display_name(&self) -> &str {
        if crate::theme::is_clamped() {
            return "claude (MAX)";
        }
        let model = self.current_model();
        if model.is_empty() {
            DEFAULT_MODEL_DISPLAY_NAME
        } else {
            model
        }
    }

    /// Get the label for the current collaboration mode.
    pub(crate) fn collaboration_mode_label(&self) -> Option<&'static str> {
        if !self.collaboration_modes_enabled() {
            return None;
        }
        let active_mode = self.active_mode_kind();
        active_mode
            .is_tui_visible()
            .then_some(active_mode.display_name())
    }

    fn collaboration_mode_indicator(&self) -> Option<CollaborationModeIndicator> {
        if !self.collaboration_modes_enabled() {
            return None;
        }
        let effective_mode = self.effective_collaboration_mode();
        let kind = effective_mode.mode;
        if !kind.is_tui_visible() {
            return None;
        }
        let effort_label = match effective_mode.reasoning_effort() {
            Some(ReasoningEffortConfig::None) | None => None,
            Some(effort) => Some(effort.to_string()),
        };
        let model_label = if crate::theme::is_clamped() {
            "claude (MAX)".to_string()
        } else if effective_mode.model().is_empty() {
            DEFAULT_MODEL_DISPLAY_NAME.to_string()
        } else {
            effective_mode.model().to_string()
        };
        Some(CollaborationModeIndicator::new(
            kind,
            model_label,
            effort_label,
        ))
    }

    pub(crate) fn update_collaboration_mode_indicator(&mut self) {
        let indicator = self.collaboration_mode_indicator();
        self.bottom_pane.set_collaboration_mode_indicator(indicator);
    }

    pub(crate) fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    pub(crate) fn personality_description(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "No personality instructions.",
            Personality::Friendly => "Warm, collaborative, and helpful.",
            Personality::Pragmatic => "Concise, task-focused, and direct.",
        }
    }

    /// Cycle to the next collaboration mode variant (Plan -> Default -> Plan).
    pub(crate) fn cycle_collaboration_mode(&mut self) {
        if !self.collaboration_modes_enabled() {
            return;
        }

        if let Some(next_mask) = collaboration_modes::next_mask(
            self.models_manager.as_ref(),
            self.active_collaboration_mask.as_ref(),
        ) {
            self.set_collaboration_mask(next_mask);
        }
    }

    /// Update the active collaboration mask.
    ///
    /// When collaboration modes are enabled and a preset is selected,
    /// the current mode is attached to submissions as `Op::UserTurn { collaboration_mode: Some(...) }`.
    pub fn set_collaboration_mask(&mut self, mut mask: CollaborationModeMask) {
        if !self.collaboration_modes_enabled() {
            return;
        }
        if mask.mode == Some(ModeKind::Plan)
            && let Some(effort) = self.config.plan_mode_reasoning_effort
        {
            mask.reasoning_effort = Some(Some(effort));
        }
        self.active_collaboration_mask = Some(mask);
        self.update_collaboration_mode_indicator();
        self.refresh_model_display();
        self.refresh_status_line();
        self.request_redraw();
    }

    /// Return a reference to the widget's current config (includes any
    /// runtime overrides applied via TUI, e.g., model or approval policy).
    pub fn config_ref(&self) -> &Config {
        &self.config
    }
}
