use super::super::ChatWidget;
use crate::app_event::AppEvent;
use crate::bottom_pane::{
    BottomPaneView, ListSelectionView, ReflexSetupForm, SelectionItem, SelectionViewParams,
};
use chaos_ipc::ProcessId;
use chaos_kern::config::ReflexBackendSettings;
use chaos_kern::reflex::configuration;
use chaos_kern::reflex::diagnostics;
use std::sync::Arc;

impl ChatWidget {
    pub(crate) fn test_reflex(&mut self) {
        let config = self.config.clone();
        let auth = self.auth_manager.clone();
        let tx = self.app_event_tx.clone();
        let process_id = self.process_id;
        self.add_info_message(
            "Testing reflex with a synthetic read-only action…".into(),
            Some("No chat history or file contents are sent. No tool is executed.".into()),
        );
        tokio::spawn(async move {
            let result = diagnostics::test(&config, Some(&auth))
                .await
                .map_err(|err| format!("{err:#}"));
            tx.send(AppEvent::ReflexTestFinished { process_id, result });
        });
    }

    pub fn reflex_test_finished(
        &mut self,
        process_id: Option<ProcessId>,
        result: Result<diagnostics::TestReport, String>,
    ) {
        if process_id != self.process_id {
            return;
        }
        match result {
            Ok(report) => self.add_info_message(report.to_string(), None),
            Err(err) => self.add_error_message(err),
        }
        self.request_redraw();
    }

    pub(crate) fn open_reflex_popup(&mut self) {
        self.app_event_tx.send(AppEvent::OpenReflexPopup);
    }

    pub fn reflex_picker_view(&self) -> Box<dyn BottomPaneView> {
        let existing = self
            .config
            .reflex
            .iter()
            .map(|(name, settings)| (format!("Edit {name}"), name.clone(), settings.clone()));
        let presets = configuration::presets()
            .into_iter()
            .map(|(name, settings)| {
                let mut unused_name = name.to_string();
                let mut suffix = 2;
                while self.config.reflex.contains_key(&unused_name) {
                    unused_name = format!("{name}-{suffix}");
                    suffix += 1;
                }
                (format!("Add {name}"), unused_name, settings)
            });
        let items = existing
            .chain(presets)
            .map(|(label, name, settings)| SelectionItem {
                name: label,
                description: Some(format!("{:?} · {}", settings.kind, settings.model())),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenReflexSetup {
                        name: name.clone(),
                        settings: settings.clone(),
                    })
                })],
                dismiss_on_select: false,
                ..Default::default()
            })
            .collect();
        Box::new(ListSelectionView::new(
            SelectionViewParams {
                title: Some("Reflex backends".into()),
                subtitle: Some("Settings in the database · new keys in the OS keyring".into()),
                footer_note: Some(
                    "Action-risk checks send conversation and tool arguments to the backend."
                        .into(),
                ),
                footer_hint: Some("↑/↓ choose · Enter configure · Esc close".into()),
                items,
                ..Default::default()
            },
            self.app_event_tx.clone(),
        ))
    }

    pub fn reflex_setup_view(
        &self,
        name: String,
        mut settings: ReflexBackendSettings,
    ) -> Box<dyn BottomPaneView> {
        if settings.api_key.is_none()
            && settings.auth_provider.is_none()
            && settings.env_key.is_none()
        {
            let mut accounts: Vec<_> = self
                .config
                .model_providers
                .keys()
                .filter(|id| {
                    let mut candidate = settings.clone();
                    candidate.auth_provider = Some((*id).clone());
                    configuration::validate(&self.config, &candidate).is_ok()
                        && self
                            .auth_manager
                            .auth_for_provider(id)
                            .is_some_and(|auth| auth.api_key().is_some())
                })
                .cloned()
                .collect();
            if accounts.len() == 1 {
                settings.auth_provider = accounts.pop();
            }
        }
        Box::new(ReflexSetupForm::new(
            Arc::new(self.config.clone()),
            self.auth_manager.clone(),
            self.process_id,
            name,
            settings,
            self.app_event_tx.clone(),
        ))
    }

    pub fn reflex_backend_saved(&mut self, name: String, settings: ReflexBackendSettings) {
        self.config.reflex.insert(name, settings);
        self.request_redraw();
    }
}
