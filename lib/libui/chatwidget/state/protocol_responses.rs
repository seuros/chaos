//! Protocol response handlers: all_tools, mcp_tools, custom_prompts, connectors
//! snapshot updates, and connector enable/disable helpers.
use super::super::*;

impl ChatWidget {
    pub(crate) fn on_all_tools_response(&mut self, ev: chaos_ipc::protocol::AllToolsResponseEvent) {
        // Forward to the app layer where the TileManager can open/populate the tools pane.
        self.app_event_tx.send(AppEvent::AllToolsReceived(ev));
    }

    pub(crate) fn on_list_mcp_tools(&mut self, ev: McpListToolsResponseEvent) {
        self.add_to_history(history_cell::new_mcp_tools_output(
            &self.config,
            ev.tools,
            ev.resources,
            ev.resource_templates,
            &ev.auth_statuses,
        ));
    }

    pub(crate) fn on_list_custom_prompts(&mut self, ev: ListCustomPromptsResponseEvent) {
        let len = ev.custom_prompts.len();
        debug!("received {len} custom prompts");
        // Forward to bottom pane so the slash popup can show them now.
        self.bottom_pane.set_custom_prompts(ev.custom_prompts);
    }

    pub fn on_connectors_loaded(
        &mut self,
        result: Result<ConnectorsSnapshot, String>,
        is_final: bool,
    ) {
        let mut trigger_pending_force_refetch = false;
        if is_final {
            self.connectors_prefetch_in_flight = false;
            if self.connectors_force_refetch_pending {
                self.connectors_force_refetch_pending = false;
                trigger_pending_force_refetch = true;
            }
        }

        match result {
            Ok(mut snapshot) => {
                // Connectors infrastructure removed — pass through as-is.
                if let ConnectorsCacheState::Ready(existing_snapshot) = &self.connectors_cache {
                    let enabled_by_id: HashMap<&str, bool> = existing_snapshot
                        .connectors
                        .iter()
                        .map(|connector| (connector.id.as_str(), connector.is_enabled))
                        .collect();
                    for connector in &mut snapshot.connectors {
                        if let Some(is_enabled) = enabled_by_id.get(connector.id.as_str()) {
                            connector.is_enabled = *is_enabled;
                        }
                    }
                }
                if is_final {
                    self.connectors_partial_snapshot = None;
                    self.refresh_connectors_popup_if_open(&snapshot.connectors);
                    self.connectors_cache = ConnectorsCacheState::Ready(snapshot.clone());
                } else {
                    self.connectors_partial_snapshot = Some(snapshot.clone());
                }
                self.bottom_pane.set_connectors_snapshot(Some(snapshot));
            }
            Err(err) => {
                let partial_snapshot = self.connectors_partial_snapshot.take();
                if let ConnectorsCacheState::Ready(snapshot) = &self.connectors_cache {
                    warn!("failed to refresh apps list; retaining current apps snapshot: {err}");
                    self.bottom_pane
                        .set_connectors_snapshot(Some(snapshot.clone()));
                } else if let Some(snapshot) = partial_snapshot {
                    warn!(
                        "failed to load full apps list; falling back to installed apps snapshot: {err}"
                    );
                    self.refresh_connectors_popup_if_open(&snapshot.connectors);
                    self.connectors_cache = ConnectorsCacheState::Ready(snapshot.clone());
                    self.bottom_pane.set_connectors_snapshot(Some(snapshot));
                } else {
                    self.connectors_cache = ConnectorsCacheState::Failed(err);
                    self.bottom_pane.set_connectors_snapshot(/*snapshot*/ None);
                }
            }
        }

        if trigger_pending_force_refetch {
            self.prefetch_connectors_with_options(/*force_refetch*/ true);
        }
    }

    pub fn update_connector_enabled(&mut self, connector_id: &str, enabled: bool) {
        let ConnectorsCacheState::Ready(mut snapshot) = self.connectors_cache.clone() else {
            return;
        };

        let mut changed = false;
        for connector in &mut snapshot.connectors {
            if connector.id == connector_id {
                changed = connector.is_enabled != enabled;
                connector.is_enabled = enabled;
                break;
            }
        }

        if !changed {
            return;
        }

        self.refresh_connectors_popup_if_open(&snapshot.connectors);
        self.connectors_cache = ConnectorsCacheState::Ready(snapshot.clone());
        self.bottom_pane.set_connectors_snapshot(Some(snapshot));
    }
}
