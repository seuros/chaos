//! Operator-only reconciliation, never an executor restart or a force-unlock.
use chaos_journald::{
    ExecutionResolution, ExecutionResolutionRecord, ExecutionSession, ExecutionSessionState,
};
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveRequest {
    pub resolution_id: Uuid,
    pub expected_process_id: Option<chaos_ipc::ProcessId>,
    pub expected_turn_id: Option<Uuid>,
    pub expected_recovery_epoch: u64,
    pub containment_confirmed: bool,
    pub actor: String,
    pub reason: String,
}

impl ResolveRequest {
    fn resolution(&self, outcome: Option<serde_json::Value>) -> ExecutionResolution {
        ExecutionResolution {
            resolution_id: self.resolution_id,
            expected_process_id: self.expected_process_id,
            expected_turn_id: self.expected_turn_id,
            expected_recovery_epoch: self.expected_recovery_epoch,
            containment_confirmed: self.containment_confirmed,
            actor: self.actor.clone(),
            reason: self.reason.clone(),
            outcome,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RecoveryView {
    session: ExecutionSession,
    resolution: Option<ExecutionResolutionRecord>,
    supervisor_stopped: bool,
    can_resolve: bool,
}

impl Owner {
    async fn resolution_record(&self) -> Result<Option<ExecutionResolutionRecord>, ApiError> {
        match self
            .store
            .call(Request::GetResolution {
                session_id: self.id,
            })
            .await?
        {
            Response::Resolution(record) => Ok(record),
            _ => Err(unavailable()),
        }
    }

    async fn session_record(&self) -> Result<ExecutionSession, ApiError> {
        match self
            .store
            .call(Request::GetSession {
                session_id: self.id,
            })
            .await?
        {
            Response::Session(record) => Ok(record),
            _ => Err(unavailable()),
        }
    }
}

impl Session {
    pub(crate) async fn recovery_view(&self) -> Result<RecoveryView, ApiError> {
        let owner = self.ledger.get().ok_or_else(|| {
            ApiError::conflict("recovery requires a persisted session; restart to reconcile")
        })?;
        let mut session = owner.session_record().await?;
        let resolution = owner.resolution_record().await?;
        if let Some(record) = &resolution {
            // Resolution closes permanently. If it committed between these
            // reads, don't present its audit alongside a stale unknown state.
            session.state = ExecutionSessionState::Closed;
            session.active_turn_id = None;
            session.recovery_epoch = record.request.expected_recovery_epoch;
        }
        let supervisor_stopped = self.commands.is_closed();
        let can_resolve = supervisor_stopped
            && session.state == ExecutionSessionState::Unknown
            && owner.lease().is_ok();
        Ok(RecoveryView {
            session,
            resolution,
            supervisor_stopped,
            can_resolve,
        })
    }

    pub(crate) async fn resolve(
        &self,
        request: ResolveRequest,
    ) -> Result<ExecutionResolutionRecord, ApiError> {
        request.resolution(None).validate().map_err(|_| {
            ApiError::bad_request(
                "resolution requires containment confirmation and bounded actor/reason",
            )
        })?;
        let _admission = self.admission.lock().await;
        self.notify();
        let owner = self.ledger.get().ok_or_else(|| {
            ApiError::conflict("recovery requires a persisted session; restart to reconcile")
        })?;
        // Even a caller's assertion cannot let a still-running supervisor
        // dispatch queued input after its reservation has been released.
        if !self.commands.is_closed() {
            return Err(ApiError::conflict(
                "supervisor is still present; stop the daemon and recover before resolving",
            ));
        }
        let record = if let Some(record) = owner.resolution_record().await? {
            if record.request != request.resolution(record.request.outcome.clone()) {
                return Err(ApiError::conflict("execution resolution is immutable"));
            }
            record
        } else {
            let persisted = owner.session_record().await?;
            if persisted.state != ExecutionSessionState::Unknown
                || persisted.process_id != request.expected_process_id
                || persisted.active_turn_id != request.expected_turn_id
                || persisted.recovery_epoch != request.expected_recovery_epoch
            {
                return Err(ApiError::conflict(
                    "resolution target changed or is not quarantined",
                ));
            }
            let outcome = if let Some(id) = request.expected_turn_id {
                let turn = owner.get_turn(&id.to_string()).await?;
                let mut snapshot = snapshot(&turn)?;
                snapshot.status = TurnStatus::Interrupted;
                let snapshot = bounded_snapshot(snapshot).unwrap_or_else(|_| TurnSnapshot {
                    turn_id: id.to_string(),
                    status: TurnStatus::Interrupted,
                    result: None,
                    result_truncated: true,
                    error: Some(
                        "Prior partial snapshot omitted because it exceeds recovery bounds".into(),
                    ),
                    usage: None,
                });
                Some(serde_json::to_value(snapshot).unwrap())
            } else {
                None
            };
            match owner
                .write(Request::Resolve {
                    lease: owner.lease()?,
                    resolution: request.resolution(outcome),
                })
                .await?
            {
                Response::Resolved(record) => record,
                _ => {
                    owner.abandon();
                    return Err(unavailable());
                }
            }
        };
        // A failed/ambiguous response retains capacity. Identical retries may
        // read the committed audit and reach here even after ownership expired.
        let turn = if let Some(id) = record.request.expected_turn_id {
            let turn = owner.get_turn(&id.to_string()).await?;
            let snapshot = snapshot(&turn)?;
            if snapshot.status != TurnStatus::Interrupted {
                return Err(unavailable());
            }
            Some((id.to_string(), turn, snapshot))
        } else {
            None
        };
        let mut data = self.data.lock().unwrap();
        if let Some(active) = &data.active
            && Some(parse_id(&active.id)?) != record.request.expected_turn_id
        {
            return Err(ApiError::conflict(
                "local recovery target changed; restart to reconcile",
            ));
        }
        if let Some((id, turn, snapshot)) = turn {
            data.turns.insert(
                id,
                TurnRecord {
                    fingerprint: turn.fingerprint,
                    request: None,
                    snapshot,
                },
            );
        }
        data.active = None;
        data.orphan_permit = None;
        data.process_id = record.request.expected_process_id.map(|id| id.to_string());
        data.status = SessionStatus::Closed;
        *owner.lease.lock().unwrap() = None;
        drop(data);
        self.notify();
        Ok(record)
    }
}
