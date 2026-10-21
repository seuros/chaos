use protocol::schema::v1::{
    ClientCapabilities, SessionConfigKind, SessionConfigOption, SessionUpdate,
};

use crate::state::State;
use crate::{SessionInfo, protocol};

fn boolean_supported(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .session
        .as_ref()
        .and_then(|session| session.config_options.as_ref())
        .and_then(|options| options.boolean.as_ref())
        .is_some()
}

fn filter_options(options: &mut Vec<SessionConfigOption>, boolean: bool) {
    if !boolean {
        options.retain(|option| !matches!(option.kind, SessionConfigKind::Boolean(_)));
    }
}

impl State {
    pub(crate) fn supports_boolean(&self) -> bool {
        self.client_capabilities
            .get()
            .is_some_and(boolean_supported)
    }

    pub(crate) fn filter_info(&self, info: &mut SessionInfo) {
        if let Some(options) = &mut info.config_options {
            filter_options(options, self.supports_boolean());
        }
    }

    /// Suppress optional events the client cannot consume, including replay.
    pub(crate) fn filter_update(&self, mut update: SessionUpdate) -> Option<SessionUpdate> {
        let session = self
            .client_capabilities
            .get()
            .and_then(|capabilities| capabilities.session.as_ref());
        match &mut update {
            SessionUpdate::Notice(_) if session.is_none_or(|session| session.notices.is_none()) => {
                return None;
            }
            SessionUpdate::CompactionUpdate(_) | SessionUpdate::CompactionSummaryChunk(_)
                if session.is_none_or(|session| session.compaction.is_none()) =>
            {
                return None;
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                filter_options(&mut update.config_options, self.supports_boolean());
            }
            _ => {}
        }
        Some(update)
    }
}
