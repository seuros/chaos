use crate::state::invalid;
use crate::{ClientHandle, protocol};
use protocol::schema::v1::{
    CompleteElicitationNotification, CreateElicitationRequest, CreateElicitationResponse,
    ElicitationAction, ElicitationId, ElicitationMode, ElicitationScope,
};
use std::sync::{Arc, PoisonError, Weak};

/// A reservation survives the request only after a valid URL acceptance.
/// Dropping the request future must release admission too.
struct UrlReservation {
    state: Weak<crate::state::State>,
    id: ElicitationId,
    ticket: Arc<()>,
    retained: bool,
}

impl Drop for UrlReservation {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        if let Some(state) = self.state.upgrade() {
            let mut ids = state
                .url_elicitations
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if ids
                .get(&self.id)
                .is_some_and(|pending| Arc::ptr_eq(&pending.ticket, &self.ticket))
            {
                ids.remove(&self.id);
            }
        }
    }
}

impl ClientHandle {
    /// Ask the client for structured user input in this session. The backend
    /// must validate accepted form content against its schema before using it.
    /// Unsupported modes error; cancellation/deadline never becomes acceptance.
    pub async fn create_elicitation(
        &self,
        request: CreateElicitationRequest,
    ) -> protocol::Result<CreateElicitationResponse> {
        if !matches!(request.scope(), ElicitationScope::Session(scope)
            if scope.session_id == self.session_id)
        {
            return Err(invalid("elicitation must belong to this ACP session"));
        }
        let capabilities = self.capabilities()?.elicitation;
        let supported = capabilities
            .as_ref()
            .is_some_and(|caps| match &request.mode {
                ElicitationMode::Form(_) => caps.form.is_some(),
                ElicitationMode::Url(_) => caps.url.is_some(),
                _ => false,
            });
        if !supported {
            return Err(invalid("elicitation mode was not negotiated"));
        }
        let url = matches!(request.mode, ElicitationMode::Url(_));
        let cancel = || CreateElicitationResponse::new(ElicitationAction::Cancel);
        if self.turn.cancel.is_cancelled() {
            return Ok(cancel());
        }
        let mut reservation = if let ElicitationMode::Url(mode) = &request.mode {
            let (state, slot) = self.ready()?;
            let mut ids = state
                .url_elicitations
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            ids.retain(|_, pending| {
                pending
                    .slot
                    .upgrade()
                    .is_some_and(|slot| !slot.is_invalid())
            });
            if ids.contains_key(&mode.elicitation_id) || ids.len() >= state.limits.pending_requests
            {
                return Err(invalid(
                    "URL elicitation ID is outstanding or its budget is exhausted",
                ));
            }
            let ticket = Arc::new(());
            ids.insert(
                mode.elicitation_id.clone(),
                crate::state::UrlElicitation {
                    slot: Arc::downgrade(&slot),
                    ticket: ticket.clone(),
                },
            );
            Some(UrlReservation {
                state: Arc::downgrade(&state),
                id: mode.elicitation_id.clone(),
                ticket,
                retained: false,
            })
        } else {
            None
        };
        let response = match self.client_request(request, false).await {
            Ok(response) => response,
            Err(_) => return Ok(cancel()),
        };
        match &response.action {
            ElicitationAction::Accept(accepted) if !url || accepted.content.is_none() => {
                if let Some(reservation) = &mut reservation {
                    reservation.retained = true;
                }
                Ok(response)
            }
            ElicitationAction::Decline | ElicitationAction::Cancel => Ok(response),
            _ => Ok(cancel()),
        }
    }

    /// Notify the UI that a previously issued URL elicitation completed.
    /// Unlike creating a request, completion belongs to the session, not its turn.
    pub fn complete_elicitation(&self, id: impl Into<ElicitationId>) -> protocol::Result<()> {
        let (state, slot) = self.session_ready()?;
        if !state
            .client_capabilities
            .get()
            .cloned()
            .unwrap_or_default()
            .elicitation
            .is_some_and(|caps| caps.url.is_some())
        {
            return Err(invalid("URL elicitation was not negotiated"));
        }
        let id = id.into();
        let mut ids = state
            .url_elicitations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !ids
            .get(&id)
            .and_then(|owner| owner.slot.upgrade())
            .is_some_and(|owner| Arc::ptr_eq(&owner, &slot))
        {
            return Err(invalid(
                "URL elicitation is unknown, complete or owned by another session",
            ));
        }
        self.connection
            .send_notification(CompleteElicitationNotification::new(id.clone()))?;
        ids.remove(&id);
        Ok(())
    }
}
