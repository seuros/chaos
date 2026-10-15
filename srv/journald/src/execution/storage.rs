use super::{
    ExecutionLease, ExecutionRequest, ExecutionResolutionRecord, ExecutionResponse,
    ExecutionSession, ExecutionSessionState, ExecutionTurn, ExecutionTurnState,
    MAX_EXECUTION_TURNS, conflict, decode, decode_state, invalid, parse_uuid,
};
use crate::JournalError;

pub(super) type SessionRow = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    String,
    i64,
    i64,
    i64,
);
type TurnRow = (String, String, String, Option<String>);

macro_rules! session_select_sql {
    ($predicate:literal, $lock:literal) => {
        concat!(
            "SELECT session_id, scope, process_id, state, active_turn_id,
             owner_id, lease_token, expires_at_ms,
             (SELECT COUNT(*) FROM execution_turns t
              WHERE t.session_id = execution_sessions.session_id), recovery_epoch
             FROM execution_sessions ",
            $predicate,
            $lock
        )
    };
}

const SELECT_TURN: &str = "SELECT turn_id, fingerprint, state, outcome_json FROM execution_turns
     WHERE session_id = $1 AND turn_id = $2";
const SELECT_RESOLUTION: &str =
    "SELECT request_json, resolved_at_ms FROM execution_resolutions WHERE session_id = $1";

fn resolution(row: (String, i64)) -> Result<ExecutionResolutionRecord, JournalError> {
    Ok(ExecutionResolutionRecord {
        request: decode(&row.0)?,
        resolved_at_ms: row.1,
    })
}

pub(super) fn session(row: &SessionRow) -> Result<ExecutionSession, JournalError> {
    Ok(ExecutionSession {
        session_id: parse_uuid(row.0.clone())?,
        scope: row.1.clone(),
        process_id: row
            .2
            .as_ref()
            .map(|id| {
                chaos_ipc::ProcessId::from_string(id).map_err(|source| {
                    JournalError::InvalidProcessId {
                        value: id.clone(),
                        source,
                    }
                })
            })
            .transpose()?,
        state: decode_state(row.3.clone())?,
        active_turn_id: row.4.clone().map(parse_uuid).transpose()?,
        turn_count: u64::try_from(row.8).map_err(|_| invalid("negative execution turn count"))?,
        recovery_epoch: u64::try_from(row.9)
            .map_err(|_| invalid("negative execution recovery epoch"))?,
    })
}

fn next_epoch(row: &SessionRow) -> Result<i64, JournalError> {
    row.9
        .checked_add(1)
        .ok_or_else(|| invalid("execution recovery epoch exhausted"))
}

fn lease(row: &SessionRow) -> Result<ExecutionLease, JournalError> {
    Ok(ExecutionLease {
        session_id: parse_uuid(row.0.clone())?,
        owner_id: row.5.clone(),
        token: parse_uuid(row.6.clone())?,
        expires_at_ms: row.7,
    })
}

fn turn(row: TurnRow) -> Result<ExecutionTurn, JournalError> {
    Ok(ExecutionTurn {
        turn_id: parse_uuid(row.0)?,
        fingerprint: row.1,
        state: decode_state(row.2)?,
        outcome: row.3.as_deref().map(decode).transpose()?,
    })
}

pub(super) fn check_lease(
    row: &SessionRow,
    expected: &ExecutionLease,
    now: i64,
) -> Result<(), JournalError> {
    let actual = lease(row)?;
    // Caller-provided expires_at_ms is informational, never an authority.
    if actual.owner_id != expected.owner_id
        || actual.token != expected.token
        || actual.expires_at_ms <= now
    {
        return Err(conflict("execution lease expired or writer was fenced"));
    }
    Ok(())
}

fn not_found() -> JournalError {
    JournalError::ExecutionNotFound
}

fn unique_error(error: sqlx::Error, message: &str) -> JournalError {
    if let sqlx::Error::Database(db) = &error
        && db.is_unique_violation()
    {
        return conflict(message);
    }
    error.into()
}

macro_rules! impl_storage {
    ($store:ty, $begin:expr, $lock:literal, $clock:literal, $replay:ident, $retention:ident) => {
        impl $store {
            pub(crate) async fn execute_ledger(
                &self,
                request: ExecutionRequest,
            ) -> Result<ExecutionResponse, JournalError> {
                request.validate()?;
                let mut claim_policy = None;
                let mut new_process = false;
                let mut metered = false;
                let request = match request {
                    ExecutionRequest::Replay { request } => return super::replay::$replay(self.pool(), request).await.map(ExecutionResponse::Replay),
                    ExecutionRequest::Retention { request } => return super::retention::$retention(self.pool(), &request).await.map(ExecutionResponse::Retention),
                    ExecutionRequest::Billing { request } => return self.execute_billing(request).await.map(ExecutionResponse::Billing),
                    ExecutionRequest::Invocation { request } => return self.execute_invocation(request).await,
                    ExecutionRequest::AcceptMetered { lease, turn_id, fingerprint } => {
                        metered = true;
                        ExecutionRequest::Accept { lease, turn_id, fingerprint }
                    }
                    ExecutionRequest::Delivery { request } => return self.execute_delivery(request).await,
                    ExecutionRequest::GetScope { scope } => {
                        let digest = sqlx::query_scalar::<_, Option<String>>(
                            "SELECT policy_digest FROM execution_scopes WHERE scope = $1",
                        ).bind(scope).fetch_optional(self.pool()).await?.flatten();
                        return Ok(ExecutionResponse::Scope(digest));
                    }
                    ExecutionRequest::BindScope { scope, policy_digest } => {
                        let mut tx = self.pool().begin_with($begin).await?;
                        sqlx::query(
                            "INSERT INTO execution_scopes (scope, policy_digest) VALUES ($1, NULL)
                             ON CONFLICT(scope) DO NOTHING",
                        ).bind(&scope).execute(&mut *tx).await?;
                        let existing = sqlx::query_scalar::<_, Option<String>>(concat!(
                            "SELECT policy_digest FROM execution_scopes WHERE scope = $1", $lock
                        )).bind(&scope).fetch_one(&mut *tx).await?;
                        if let Some(existing) = existing {
                            if existing != policy_digest {
                                return Err(conflict("execution scope policy is immutable"));
                            }
                        } else {
                            let count: i64 = sqlx::query_scalar(
                                "SELECT COUNT(*) FROM execution_sessions WHERE scope = $1",
                            ).bind(&scope).fetch_one(&mut *tx).await?;
                            if count != 0 {
                                return Err(conflict("legacy scope cannot adopt an unverified policy"));
                            }
                            sqlx::query("UPDATE execution_scopes SET policy_digest = $1 WHERE scope = $2")
                                .bind(&policy_digest).bind(&scope).execute(&mut *tx).await?;
                        }
                        tx.commit().await?;
                        return Ok(ExecutionResponse::Scope(Some(policy_digest)));
                    }
                    ExecutionRequest::ClaimBound { session_id, scope, policy_digest, owner_id, ttl_ms } => {
                        claim_policy = Some(policy_digest);
                        ExecutionRequest::Claim { session_id, scope, owner_id, ttl_ms }
                    }
                    ExecutionRequest::BindNewProcess { lease, process_id } => {
                        new_process = true;
                        ExecutionRequest::BindProcess { lease, process_id }
                    }
                    request => request,
                };
                match &request {
                    ExecutionRequest::GetResolution { session_id } => {
                        let record = sqlx::query_as::<_, (String, i64)>(SELECT_RESOLUTION)
                            .bind(session_id.to_string())
                            .fetch_optional(self.pool())
                            .await?
                            .map(resolution)
                            .transpose()?;
                        return Ok(ExecutionResponse::Resolution(record));
                    }
                    ExecutionRequest::GetSession { session_id } => {
                        let row = sqlx::query_as::<_, SessionRow>(session_select_sql!(
                            "WHERE session_id = $1", ""
                        ))
                            .bind(session_id.to_string())
                            .fetch_optional(self.pool())
                            .await?
                            .ok_or_else(not_found)?;
                        return Ok(ExecutionResponse::Session(session(&row)?));
                    }
                    ExecutionRequest::GetTurn { session_id, turn_id } => {
                        let row = sqlx::query_as::<_, TurnRow>(SELECT_TURN)
                            .bind(session_id.to_string())
                            .bind(turn_id.to_string())
                            .fetch_optional(self.pool())
                            .await?
                            .ok_or_else(not_found)?;
                        return Ok(ExecutionResponse::Turn(turn(row)?));
                    }
                    ExecutionRequest::ListSessions { scope, after, limit } => {
                        let rows = sqlx::query_as::<_, SessionRow>(session_select_sql!(
                            "WHERE scope = $1 AND session_id > $2 ORDER BY session_id LIMIT $3", ""
                        ))
                        .bind(scope)
                        .bind(after.map(|id| id.to_string()).unwrap_or_default())
                        .bind(i64::from(*limit))
                        .fetch_all(self.pool())
                        .await?;
                        return Ok(ExecutionResponse::Sessions(
                            rows.iter().map(session).collect::<Result<_, _>>()?,
                        ));
                    }
                    _ => {}
                }

                let id = request.session_id()
                    .ok_or_else(|| invalid("execution mutation requires a session id"))?
                    .to_string();
                let mut tx = self.pool().begin_with($begin).await?;
                if let ExecutionRequest::Claim { scope, .. } = &request {
                    // Serialize policy bootstrap with ALL claims, including
                    // legacy clients. No claim can bypass a bound namespace.
                    sqlx::query(
                        "INSERT INTO execution_scopes (scope, policy_digest) VALUES ($1, NULL)
                         ON CONFLICT(scope) DO NOTHING",
                    ).bind(scope).execute(&mut *tx).await?;
                    let binding = sqlx::query_scalar::<_, Option<String>>(concat!(
                        "SELECT policy_digest FROM execution_scopes WHERE scope = $1", $lock
                    )).bind(scope).fetch_one(&mut *tx).await?;
                    if binding != claim_policy {
                        return Err(conflict("execution scope policy does not match"));
                    }
                    // ON CONFLICT waits for a competing creator on PostgreSQL.
                    // Lease acquisition still happens only under the row lock.
                    sqlx::query(
                        "INSERT INTO execution_sessions
                         (session_id, scope, state, owner_id, lease_token, expires_at_ms)
                         VALUES ($1, $2, 'idle', '', $3, 0)
                         ON CONFLICT(session_id) DO NOTHING",
                    )
                    .bind(&id)
                    .bind(scope)
                    .bind(uuid::Uuid::new_v4().to_string())
                    .execute(&mut *tx)
                    .await?;
                }
                let mut row = sqlx::query_as::<_, SessionRow>(session_select_sql!(
                    "WHERE session_id = $1", $lock
                ))
                    .bind(&id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(not_found)?;
                // Read the storage clock AFTER acquiring the write lock.
                let now: i64 = sqlx::query_scalar($clock).fetch_one(&mut *tx).await?;
                let current = session(&row)?;

                let response = match request {
                    ExecutionRequest::Claim { scope, owner_id, ttl_ms, .. } => {
                        if current.scope != scope {
                            return Err(conflict("execution session belongs to another scope"));
                        }
                        if row.7 > now {
                            if row.5 != owner_id {
                                return Err(conflict("execution session already has a live owner"));
                            }
                            ExecutionResponse::Claimed { session: current, lease: lease(&row)? }
                        } else {
                            let token = uuid::Uuid::new_v4();
                            let expires = now + ttl_ms as i64;
                            if let Some(turn_id) = current.active_turn_id {
                                super::invocations::fence_invocations!(tx, &id, turn_id.to_string(), now);
                                sqlx::query(
                                    "UPDATE execution_turns SET state = 'unknown'
                                     WHERE session_id = $1 AND turn_id = $2",
                                )
                                .bind(&id)
                                .bind(turn_id.to_string())
                                .execute(&mut *tx)
                                .await?;
                                row.3 = "unknown".into();
                            }
                            // An expired owner may have launched autonomous
                            // work even without an HTTP turn. Only an orderly
                            // release (deadline 0), or no bound process, makes
                            // idle handoff safe without explicit containment.
                            if row.7 != 0
                                && current.process_id.is_some()
                                && current.state == ExecutionSessionState::Idle
                            {
                                row.3 = "unknown".into();
                            }
                            if row.3 == "unknown" {
                                row.9 = next_epoch(&row)?;
                            }
                            sqlx::query(
                                "UPDATE execution_sessions SET owner_id = $1, lease_token = $2,
                                 expires_at_ms = $3, state = $4, recovery_epoch = $5 WHERE session_id = $6",
                            )
                            .bind(&owner_id)
                            .bind(token.to_string())
                            .bind(expires)
                            .bind(&row.3)
                            .bind(row.9)
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                            row.5 = owner_id;
                            row.6 = token.to_string();
                            row.7 = expires;
                            ExecutionResponse::Claimed { session: session(&row)?, lease: lease(&row)? }
                        }
                    }
                    ExecutionRequest::Heartbeat { lease: expected, ttl_ms } => {
                        check_lease(&row, &expected, now)?;
                        row.7 = now + ttl_ms as i64;
                        sqlx::query("UPDATE execution_sessions SET expires_at_ms = $1 WHERE session_id = $2")
                            .bind(row.7)
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                        ExecutionResponse::Lease(lease(&row)?)
                    }
                    ExecutionRequest::BindProcess { lease: expected, process_id } => {
                        check_lease(&row, &expected, now)?;
                        let policy = sqlx::query_scalar::<_, Option<String>>(
                            "SELECT policy_digest FROM execution_scopes WHERE scope = $1",
                        ).bind(&current.scope).fetch_one(&mut *tx).await?;
                        // A process's tenant/policy ownership survives closure.
                        // Caller-supplied or legacy history cannot manufacture it.
                        if new_process || policy.is_none() {
                            sqlx::query(
                                "INSERT INTO execution_process_bindings (process_id, scope, policy_digest)
                                 VALUES ($1, $2, $3) ON CONFLICT(process_id) DO NOTHING",
                            ).bind(process_id.to_string()).bind(&current.scope).bind(&policy)
                                .execute(&mut *tx).await?;
                        }
                        let binding = sqlx::query_as::<_, (Option<String>, Option<String>)>(concat!(
                            "SELECT scope, policy_digest FROM execution_process_bindings WHERE process_id = $1", $lock
                        )).bind(process_id.to_string()).fetch_optional(&mut *tx).await?
                            .ok_or_else(|| conflict("unverified process history cannot be resumed"))?;
                        let descendant: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_descendants WHERE process_id = $1")
                            .bind(process_id.to_string()).fetch_one(&mut *tx).await?;
                        if descendant != 0 {
                            return Err(conflict("descendant histories cannot be resumed as standalone sessions"));
                        }
                        if binding.0.as_deref() != Some(current.scope.as_str()) || binding.1 != policy {
                            return Err(conflict("process history belongs to a different tenant or policy"));
                        }
                        if let Some(previous) = current.process_id {
                            if previous != process_id {
                                return Err(conflict("execution process mapping is immutable"));
                            }
                        } else {
                            if current.state != ExecutionSessionState::Idle
                                && !(current.state == ExecutionSessionState::Unknown
                                    && current.active_turn_id.is_none())
                            {
                                return Err(conflict("only an idle or quarantined startup can bind a process"));
                            }
                            row.2 = Some(process_id.to_string());
                            sqlx::query("UPDATE execution_sessions SET process_id = $1 WHERE session_id = $2")
                                .bind(&row.2)
                                .bind(&id)
                                .execute(&mut *tx)
                                .await
                                .map_err(|error| unique_error(error,
                                    "process already belongs to a live or unknown execution session"))?;
                        }
                        ExecutionResponse::Session(session(&row)?)
                    }
                    ExecutionRequest::Accept { lease: expected, turn_id, fingerprint } => {
                        check_lease(&row, &expected, now)?;
                        let previous = sqlx::query_as::<_, TurnRow>(SELECT_TURN)
                            .bind(&id)
                            .bind(turn_id.to_string())
                            .fetch_optional(&mut *tx)
                            .await?;
                        if let Some(previous) = previous {
                            let previous = turn(previous)?;
                            if previous.fingerprint != fingerprint {
                                return Err(conflict("execution turn id reused with different payload"));
                            }
                            ExecutionResponse::Accepted { turn: previous, newly_accepted: false }
                        } else {
                            if current.state != ExecutionSessionState::Idle || current.process_id.is_none() {
                                return Err(conflict("execution session is not idle with a bound process"));
                            }
                            let count: i64 = sqlx::query_scalar(
                                "SELECT COUNT(*) FROM execution_turns WHERE session_id = $1",
                            )
                            .bind(&id)
                            .fetch_one(&mut *tx)
                            .await?;
                            if count >= MAX_EXECUTION_TURNS {
                                return Err(conflict("execution session turn limit reached"));
                            }
                            sqlx::query(
                                "INSERT INTO execution_turns (session_id, turn_id, fingerprint, state, accounting_enabled, accounting_coverage)
                                 VALUES ($1, $2, $3, 'accepted', $4, $5)",
                            )
                            .bind(&id)
                            .bind(turn_id.to_string())
                            .bind(&fingerprint)
                            .bind(i32::from(metered))
                            .bind(if metered { "root_and_descendant_logical_sampling_and_compaction" } else { "unmetered" })
                            .execute(&mut *tx)
                            .await?;
                            sqlx::query(
                                "UPDATE execution_sessions SET state = 'active', active_turn_id = $1
                                 WHERE session_id = $2",
                            )
                            .bind(turn_id.to_string())
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                            ExecutionResponse::Accepted {
                                turn: ExecutionTurn { turn_id, fingerprint, state: ExecutionTurnState::Accepted, outcome: None },
                                newly_accepted: true,
                            }
                        }
                    }
                    ExecutionRequest::Dispatch { lease: expected, turn_id } => {
                        check_lease(&row, &expected, now)?;
                        if current.state != ExecutionSessionState::Active || current.active_turn_id != Some(turn_id) {
                            return Err(conflict("execution turn is not active"));
                        }
                        let previous = sqlx::query_as::<_, TurnRow>(SELECT_TURN)
                            .bind(&id)
                            .bind(turn_id.to_string())
                            .fetch_one(&mut *tx)
                            .await?;
                        let mut previous = turn(previous)?;
                        let newly_dispatched = match previous.state {
                            ExecutionTurnState::Accepted => true,
                            ExecutionTurnState::Running => false,
                            _ => return Err(conflict("execution turn cannot be dispatched")),
                        };
                        if newly_dispatched {
                            sqlx::query(
                                "UPDATE execution_turns SET state = 'running' WHERE session_id = $1 AND turn_id = $2",
                            )
                            .bind(&id)
                            .bind(turn_id.to_string())
                            .execute(&mut *tx)
                            .await?;
                            previous.state = ExecutionTurnState::Running;
                        }
                        ExecutionResponse::Dispatched { turn: previous, newly_dispatched }
                    }
                    ExecutionRequest::Finish { lease: expected, turn_id, state, outcome } => {
                        check_lease(&row, &expected, now)?;
                        let previous = sqlx::query_as::<_, TurnRow>(SELECT_TURN)
                            .bind(&id)
                            .bind(turn_id.to_string())
                            .fetch_optional(&mut *tx)
                            .await?
                            .ok_or_else(not_found)?;
                        let mut previous = turn(previous)?;
                        if previous.state.is_terminal() {
                            if previous.state != state || previous.outcome != outcome {
                                return Err(conflict("execution terminal outcome is immutable"));
                            }
                        } else {
                            if current.active_turn_id != Some(turn_id) {
                                return Err(conflict("execution turn is not active"));
                            }
                            if previous.state == ExecutionTurnState::Accepted
                                && matches!(state, ExecutionTurnState::Completed | ExecutionTurnState::Failed)
                            {
                                return Err(conflict("undispatched execution cannot complete"));
                            }
                            let outcome_json = outcome.as_ref().map(serde_json::to_string).transpose()
                                .map_err(|source| JournalError::Serialize { field: "execution outcome", source })?;
                            sqlx::query(
                                "UPDATE execution_turns SET state = $1, outcome_json = $2
                                 WHERE session_id = $3 AND turn_id = $4",
                            )
                            .bind(state.as_str())
                            .bind(outcome_json)
                            .bind(&id)
                            .bind(turn_id.to_string())
                            .execute(&mut *tx)
                            .await?;
                            sqlx::query(
                                "UPDATE execution_sessions SET state = $1, active_turn_id = $2,
                                 recovery_epoch = $3 WHERE session_id = $4",
                            )
                            .bind(if state.is_terminal() { "idle" } else { "unknown" })
                            .bind(if state.is_terminal() { None } else { Some(turn_id.to_string()) })
                            .bind(if state == ExecutionTurnState::Unknown { next_epoch(&row)? } else { row.9 })
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                            previous.state = state;
                            previous.outcome = outcome;
                            super::invocations::fence_invocations!(tx, &id, turn_id.to_string(), now);
                            if state.is_terminal() {
                                super::outbox::enqueue!(tx, &current, Some(previous.clone()), None, now);
                            }
                        }
                        ExecutionResponse::Turn(previous)
                    }
                    ExecutionRequest::Quarantine { lease: expected } => {
                        check_lease(&row, &expected, now)?;
                        if current.state == ExecutionSessionState::Closed {
                            return Err(conflict("closed execution cannot be quarantined"));
                        }
                        if let Some(turn_id) = current.active_turn_id {
                            super::invocations::fence_invocations!(tx, &id, turn_id.to_string(), now);
                            sqlx::query(
                                "UPDATE execution_turns SET state = 'unknown' WHERE session_id = $1 AND turn_id = $2",
                            )
                            .bind(&id)
                            .bind(turn_id.to_string())
                            .execute(&mut *tx)
                            .await?;
                        }
                        row.9 = next_epoch(&row)?;
                        sqlx::query("UPDATE execution_sessions SET state = 'unknown', recovery_epoch = $1 WHERE session_id = $2")
                            .bind(row.9)
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                        row.3 = "unknown".into();
                        ExecutionResponse::Session(session(&row)?)
                    }
                    ExecutionRequest::ReconcileIdle { lease: expected } => {
                        check_lease(&row, &expected, now)?;
                        if current.state != ExecutionSessionState::Unknown || current.active_turn_id.is_some() {
                            return Err(conflict("only an unknown session without an active turn can reconcile idle"));
                        }
                        sqlx::query("UPDATE execution_sessions SET state = 'idle' WHERE session_id = $1")
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                        row.3 = "idle".into();
                        ExecutionResponse::Session(session(&row)?)
                    }
                    ExecutionRequest::Close { lease: expected } => {
                        check_lease(&row, &expected, now)?;
                        if current.active_turn_id.is_some() || current.state == ExecutionSessionState::Unknown {
                            return Err(conflict("unfinished execution cannot be closed"));
                        }
                        sqlx::query("UPDATE execution_sessions SET state = 'closed' WHERE session_id = $1")
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                        row.3 = "closed".into();
                        ExecutionResponse::Session(session(&row)?)
                    }
                    ExecutionRequest::Release { lease: expected } => {
                        check_lease(&row, &expected, now)?;
                        if current.active_turn_id.is_some() || current.state == ExecutionSessionState::Unknown {
                            return Err(conflict("unfinished execution cannot release ownership"));
                        }
                        sqlx::query("UPDATE execution_sessions SET expires_at_ms = 0 WHERE session_id = $1")
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                        ExecutionResponse::Released
                    }
                    ExecutionRequest::Resolve { lease: expected, resolution: requested } => {
                        let previous = sqlx::query_as::<_, (String, i64)>(SELECT_RESOLUTION)
                            .bind(&id)
                            .fetch_optional(&mut *tx)
                            .await?;
                        if let Some(previous) = previous {
                            let previous = resolution(previous)?;
                            if previous.request != requested {
                                return Err(conflict("execution resolution is immutable"));
                            }
                            // No writes: a lost acknowledgement can reconcile
                            // even though resolution atomically released the lease.
                            ExecutionResponse::Resolved(previous)
                        } else {
                            check_lease(&row, &expected, now)?;
                            if current.state != ExecutionSessionState::Unknown
                                || current.process_id != requested.expected_process_id
                                || current.active_turn_id != requested.expected_turn_id
                                || current.recovery_epoch != requested.expected_recovery_epoch
                            {
                                return Err(conflict("resolution target changed or is not quarantined"));
                            }
                            if let Some(turn_id) = current.active_turn_id {
                                super::invocations::fence_invocations!(tx, &id, turn_id.to_string(), now);
                                let outcome = requested.outcome.as_ref()
                                    .map(serde_json::to_string).transpose()
                                    .map_err(|source| JournalError::Serialize {
                                        field: "execution resolution outcome", source
                                    })?;
                                let changed = sqlx::query(
                                    "UPDATE execution_turns SET state = 'interrupted', outcome_json = $1
                                     WHERE session_id = $2 AND turn_id = $3 AND state = 'unknown'",
                                )
                                .bind(outcome)
                                .bind(&id)
                                .bind(turn_id.to_string())
                                .execute(&mut *tx)
                                .await?;
                                if changed.rows_affected() != 1 {
                                    return Err(conflict("resolution turn is not quarantined"));
                                }
                            }
                            let resolved_turn = if let Some(turn_id) = current.active_turn_id {
                                Some(turn(sqlx::query_as::<_, TurnRow>(SELECT_TURN)
                                    .bind(&id).bind(turn_id.to_string()).fetch_one(&mut *tx).await?)?)
                            } else { None };
                            super::outbox::enqueue!(tx, &current, resolved_turn, Some(requested.resolution_id), now);
                            sqlx::query(
                                "UPDATE execution_sessions SET state = 'closed',
                                 active_turn_id = NULL, expires_at_ms = 0 WHERE session_id = $1",
                            )
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                            let request_json = serde_json::to_string(&requested)
                                .map_err(|source| JournalError::Serialize {
                                    field: "execution resolution", source
                                })?;
                            sqlx::query(
                                "INSERT INTO execution_resolutions
                                 (session_id, resolution_id, request_json, resolved_at_ms)
                                 VALUES ($1, $2, $3, $4)",
                            )
                            .bind(&id)
                            .bind(requested.resolution_id.to_string())
                            .bind(request_json)
                            .bind(now)
                            .execute(&mut *tx)
                            .await
                            .map_err(|error| unique_error(error,
                                "resolution id already belongs to another session"))?;
                            ExecutionResponse::Resolved(ExecutionResolutionRecord {
                                request: requested,
                                resolved_at_ms: now,
                            })
                        }
                    }
                    _ => unreachable!("read requests returned before transaction"),
                };
                tx.commit().await?;
                Ok(response)
            }
        }
    };
}

impl_storage!(
    crate::SqliteJournalStore,
    "BEGIN IMMEDIATE",
    "",
    "SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 +
     CAST(substr(strftime('%f', 'now'), 4, 3) AS INTEGER)",
    execute_replay_sqlite,
    execute_retention_sqlite
);
impl_storage!(
    crate::PostgresJournalStore,
    "BEGIN",
    " FOR UPDATE",
    "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)",
    execute_replay_postgres,
    execute_retention_postgres
);
