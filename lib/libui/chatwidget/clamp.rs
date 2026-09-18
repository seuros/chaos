//! Session-local selection of first-party CLI transports.

use chaos_ipc::config_types::ClampBackend;

use super::*;

impl ChatWidget {
    /// Enable the configured backend (for example from `--clamp`).
    pub fn activate_clamp(&mut self) {
        self.set_clamp_enabled(true, self.config.clamp_backend);
    }

    pub(super) fn dispatch_clamp_command(&mut self, args: &str) {
        let (enabled, backend) = match args.trim().to_ascii_lowercase().as_str() {
            "" => (!crate::theme::is_clamped(), self.config.clamp_backend),
            "claude" | "claude-code" => (true, ClampBackend::ClaudeCode),
            "agy" | "antigravity" => (true, ClampBackend::Antigravity),
            "off" => (false, self.config.clamp_backend),
            _ => {
                self.add_error_message("Usage: /clamp [claude|agy|off]".to_string());
                return;
            }
        };
        self.set_clamp_enabled(enabled, backend);
    }

    fn set_clamp_enabled(&mut self, enabled: bool, backend: ClampBackend) {
        let was_clamped = crate::theme::is_clamped();
        let changed_backend = self.config.clamp_backend != backend;
        if enabled && !was_clamped {
            self.capture_pre_clamp_selection();
        }
        self.config.clamp = enabled;
        self.config.clamp_backend = backend;
        crate::theme::set_clamped(enabled);
        self.app_event_tx.send(AppEvent::ChaosOp(Op::SetClamped {
            enabled,
            backend: Some(backend),
        }));

        if enabled {
            if !was_clamped || changed_backend {
                let model = initial_clamp_model(
                    backend,
                    self.current_model(),
                    self.config.antigravity.resolved().model,
                );
                self.set_model(&model);
            }
            self.add_info_message(
                format!("Clamped: using {} as transport.", backend.display_name()),
                Some("Use /clamp to switch back, or /clamp claude|agy to change backend.".into()),
            );
        } else {
            self.restore_pre_clamp_selection();
            self.add_info_message("Unclamped: using direct API transport.".into(), None);
        }
        self.refresh_model_display();
        self.refresh_status_line();
        self.request_redraw();
    }

    pub(super) fn open_antigravity_model_prompt(&mut self) {
        if let Some(model) = self.config.antigravity.resolved().model {
            self.add_info_message(
                format!("Antigravity model is pinned to {model}."),
                Some("Change antigravity.model or CHAOS_AGY_MODEL and start a new session.".into()),
            );
            return;
        }
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Antigravity model".into(),
            "Enter a model slug from `agy models`".into(),
            Some(format!("Current: {}", self.current_model())),
            Box::new(move |model: String| {
                if model.chars().any(char::is_whitespace) {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(
                            "Expected one Antigravity model slug.".into(),
                        ),
                    )));
                    return;
                }
                // This selection belongs to the clamp session, not the API provider.
                tx.send(AppEvent::UpdateModel(model));
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}

fn initial_clamp_model(
    backend: ClampBackend,
    current: &str,
    antigravity_model: Option<String>,
) -> String {
    match backend {
        ClampBackend::ClaudeCode => {
            if current.starts_with("claude")
                || matches!(current, "default" | "sonnet" | "opus" | "haiku")
            {
                current.to_string()
            } else {
                "default".to_string()
            }
        }
        ClampBackend::Antigravity => antigravity_model.unwrap_or_else(|| {
            let slug = current.rsplit('/').next().unwrap_or(current);
            if slug.starts_with("gemini-") || slug.starts_with("claude-") {
                current.to_string()
            } else {
                chaos_clamp::AntigravityConfig::default().model
            }
        }),
    }
}

#[cfg(test)]
mod tests;
