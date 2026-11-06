use crate::state::invalid;
use crate::{ClientHandle, protocol};
use dedup_machines::{Claim, ClaimId, HashDedup};
use protocol::schema::v1::{
    CompleteElicitationNotification, CreateElicitationRequest, CreateElicitationResponse,
    ElicitationAction, ElicitationId, ElicitationMode, ElicitationScope, SessionId,
};
use std::sync::{Arc, PoisonError, Weak};
use std::time::Duration;
use tokio::time::Instant;

/// Outstanding URL elicitations are forgotten after this long without completion.
pub(crate) const URL_ELICITATION_TTL: Duration = Duration::from_secs(15 * 60);

pub(crate) struct UrlLedger {
    origin: Instant,
    ledger: HashDedup<(SessionId, ElicitationId)>,
}

impl UrlLedger {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            origin: Instant::now(),
            ledger: HashDedup::with_capacity(capacity),
        }
    }

    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    pub fn reserve(
        &mut self,
        session: SessionId,
        id: ElicitationId,
        mut dead: impl FnMut(&SessionId) -> bool,
    ) -> Option<ClaimId> {
        self.ledger.expire_where(|(owner, _)| dead(owner));
        match self
            .ledger
            .claim((session, id), self.now(), URL_ELICITATION_TTL)
        {
            Claim::Acquired(claim) => Some(claim),
            _ => None,
        }
    }

    pub fn retain(&mut self, claim: ClaimId) -> bool {
        self.ledger.complete(claim, self.now())
    }

    pub fn release(&mut self, claim: ClaimId) -> bool {
        self.ledger.abandon(claim)
    }

    pub fn complete(&mut self, session: &SessionId, id: &ElicitationId) -> bool {
        self.ledger
            .expire_where(|(owner, outstanding)| owner == session && outstanding == id)
            == 1
    }
}

/// A reservation survives the request only after a valid URL acceptance.
/// Dropping the request future must release admission too.
struct UrlReservation {
    state: Weak<crate::state::State>,
    claim: ClaimId,
    retained: bool,
}

impl UrlReservation {
    fn retain(&mut self) {
        self.retained = true;
        if let Some(state) = self.state.upgrade() {
            let _ = state
                .url_elicitations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(self.claim);
        }
    }
}

impl Drop for UrlReservation {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        if let Some(state) = self.state.upgrade() {
            let _ = state
                .url_elicitations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .release(self.claim);
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
            let (state, _) = self.ready()?;
            let sessions = state
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let claim = state
                .url_elicitations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .reserve(
                    self.session_id.clone(),
                    mode.elicitation_id.clone(),
                    |session| sessions.get(session).is_none_or(|slot| slot.is_invalid()),
                )
                .ok_or_else(|| {
                    invalid("URL elicitation ID is outstanding or its budget is exhausted")
                })?;
            Some(UrlReservation {
                state: Arc::downgrade(&state),
                claim,
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
                    reservation.retain();
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
        let (state, _) = self.session_ready()?;
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
        if !ids.complete(&self.session_id, &id) {
            return Err(invalid(
                "URL elicitation is unknown, complete or owned by another session",
            ));
        }
        self.connection
            .send_notification(CompleteElicitationNotification::new(id))
    }
}

#[cfg(test)]
mod tests;
