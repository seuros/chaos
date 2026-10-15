//! Explicit, non-destructive point-in-time archival of execution evidence.
//!
//! Export is not an invoice, proof of containment, or proof an external copy
//! exists. Sealing records an operator's attestation of that external copy.
//! Originals remain available to preserve exact idempotency comparisons and
//! permanent ownership. Neither operation releases history/admission limits.
use super::*;
use sha2::{Digest, Sha256};

pub const MAX_EXECUTION_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionRetentionRequest {
    Export {
        scope: String,
        session_id: Uuid,
    },
    Seal {
        scope: String,
        session_id: Uuid,
        manifest_digest: String,
        /// Nonsecret operator locator, not a URL fetched by the daemon.
        archive_reference: String,
        actor: String,
    },
    Read {
        scope: String,
        session_id: Uuid,
    },
}

impl ExecutionRetentionRequest {
    fn identity(&self) -> (&str, Uuid) {
        match self {
            Self::Export { scope, session_id }
            | Self::Seal {
                scope, session_id, ..
            }
            | Self::Read { scope, session_id } => (scope, *session_id),
        }
    }

    pub fn validate(&self) -> Result<(), JournalError> {
        let (scope, _) = self.identity();
        if scope.is_empty() || scope.len() > 256 {
            return Err(invalid("archive scope exceeds bounds"));
        }
        if let Self::Seal {
            manifest_digest,
            archive_reference,
            actor,
            ..
        } = self
        {
            if manifest_digest.len() != 64
                || !manifest_digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid("archive digest must be lowercase SHA-256"));
            }
            for (value, bound) in [(archive_reference, 1024), (actor, 256)] {
                if value.trim().is_empty()
                    || value.len() > bound
                    || value.chars().any(char::is_control)
                {
                    return Err(invalid("archive attestation exceeds bounds"));
                }
            }
        }
        Ok(())
    }
}

/// JSON bytes are hashed exactly as stored, not reserialized by the recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionArchiveManifest {
    pub sha256: String,
    pub json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionArchiveRecord {
    pub session_id: Uuid,
    pub scope: String,
    pub manifest_digest: String,
    pub exported_at_ms: i64,
    pub sealed_at_ms: Option<i64>,
    pub archive_reference: Option<String>,
    pub actor: Option<String>,
    /// Explicitly false: this implementation retains all original evidence.
    pub payloads_compacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRetentionResponse {
    pub manifest: Option<ExecutionArchiveManifest>,
    pub archive: Option<ExecutionArchiveRecord>,
}

type ArchiveRow = (
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
);

fn archive_response(session_id: Uuid, row: Option<ArchiveRow>) -> ExecutionRetentionResponse {
    let Some((scope, sha256, json, exported_at_ms, sealed_at_ms, archive_reference, actor)) = row
    else {
        return ExecutionRetentionResponse {
            manifest: None,
            archive: None,
        };
    };
    ExecutionRetentionResponse {
        manifest: Some(ExecutionArchiveManifest {
            sha256: sha256.clone(),
            json,
        }),
        archive: Some(ExecutionArchiveRecord {
            session_id,
            scope,
            manifest_digest: sha256,
            exported_at_ms,
            sealed_at_ms,
            archive_reference,
            actor,
            payloads_compacted: false,
        }),
    }
}

/// All queries execute under the execution-session writer lock. Do not include
/// renewable lease credentials or global mutable process transcripts in exports.
macro_rules! financial_snapshot {
    (sqlite, $tx:ident, $scope:ident, $session_id:ident, $estimated:ident) => {
        serde_json::Map::new()
    };
    (postgres, $tx:ident, $scope:ident, $session_id:ident, $estimated:ident) => {
        super::billing::archive_evidence(&mut $tx, $scope, &$session_id, $estimated).await?
    };
}

macro_rules! snapshot {
    ($backend:ident, $tx:ident, $session_id:ident, $scope:ident, $process:expr, $epoch:expr) => {{
        // A JSON character takes at most six bytes after escaping. Conservative
        // overhead per row bounds materialization before fetch_all allocates.
        // Reject instead of silently dropping a turn, receipt or audit record.
        let estimated: i64 = sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(estimated),0) AS BIGINT) FROM (
                SELECT CAST(2048 AS BIGINT) + 6 * (length(turn_id) + length(fingerprint) + length(state) + COALESCE(length(outcome_json),0) + length(accounting_coverage)) AS estimated
                FROM execution_turns WHERE session_id = $1
                UNION ALL SELECT CAST(2048 AS BIGINT) + 6 * (length(resolution_id) + length(request_json))
                FROM execution_resolutions WHERE session_id = $1
                UNION ALL SELECT CAST(2048 AS BIGINT) + 6 * (length(event_id) + length(event_key) + COALESCE(length(turn_id),0) + length(kind) + length(body_json))
                FROM execution_outbox WHERE session_id = $1
                UNION ALL SELECT CAST(2048 AS BIGINT) + 6 * (length(invocation_id) + length(turn_id) + length(request_json) + length(state) + COALESCE(length(receipt_json),0) + COALESCE(length(duplicate_receipt_of),0) + COALESCE(length(source_turn_id),0))
                FROM execution_invocations WHERE session_id = $1
                UNION ALL SELECT CAST(2048 AS BIGINT) + 6 * (length(d.process_id) + length(d.root_turn_id) + length(d.parent_process_id) + COALESCE(length(b.scope),0) + COALESCE(length(b.policy_digest),0))
                FROM execution_descendants d LEFT JOIN execution_process_bindings b ON b.process_id = d.process_id WHERE d.session_id = $1
                UNION ALL SELECT CAST(2048 AS BIGINT) + 6 * (length(r.receipt_key) + length(r.invocation_id))
                FROM execution_provider_receipts r JOIN execution_invocations i ON i.invocation_id = r.invocation_id WHERE i.session_id = $1
                UNION ALL SELECT CAST(2048 AS BIGINT) + 6 * length(event_json)
                FROM execution_replay_events WHERE session_id = $1
            ) archive_sizes"
        ).bind(&$session_id).fetch_one(&mut *$tx).await?;
        if estimated > MAX_EXECUTION_ARCHIVE_BYTES as i64 - 32 * 1024 {
            return Err(conflict("archive evidence exceeds bounded manifest capacity"));
        }
        let policy: Option<String> = sqlx::query_scalar(
            "SELECT policy_digest FROM execution_scopes WHERE scope = $1"
        ).bind($scope).fetch_optional(&mut *$tx).await?.flatten();
        let binding = if let Some(process) = &$process {
            sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT scope, policy_digest FROM execution_process_bindings WHERE process_id = $1"
            ).bind(process).fetch_optional(&mut *$tx).await?
        } else { None };
        let turns = sqlx::query_as::<_, (String, String, String, Option<String>, i32, String)>(
            "SELECT turn_id, fingerprint, state, outcome_json, accounting_enabled, accounting_coverage
             FROM execution_turns WHERE session_id = $1 ORDER BY turn_id"
        ).bind(&$session_id).fetch_all(&mut *$tx).await?;
        let resolutions = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT resolution_id, request_json, resolved_at_ms FROM execution_resolutions
             WHERE session_id = $1 ORDER BY resolution_id"
        ).bind(&$session_id).fetch_all(&mut *$tx).await?;
        let events = sqlx::query_as::<_, (String, String, Option<String>, String, i64, String, i64, Option<i64>)>(
            "SELECT event_id, event_key, turn_id, kind, created_at_ms, body_json, attempts, delivered_at_ms
             FROM execution_outbox WHERE session_id = $1 ORDER BY event_id"
        ).bind(&$session_id).fetch_all(&mut *$tx).await?;
        let invocations = sqlx::query_as::<_, (String, String, String, i64, String, Option<String>, Option<String>, Option<i64>, Option<String>)>(
            "SELECT invocation_id, turn_id, request_json, started_at_ms, state, receipt_json,
             duplicate_receipt_of, finished_at_ms, source_turn_id FROM execution_invocations
             WHERE session_id = $1 ORDER BY invocation_id"
        ).bind(&$session_id).fetch_all(&mut *$tx).await?;
        let descendants = sqlx::query_as::<_, (String, String, String, i64, Option<String>, Option<String>)>(
            "SELECT d.process_id,d.root_turn_id,d.parent_process_id,d.registered_at_ms,b.scope,b.policy_digest
             FROM execution_descendants d LEFT JOIN execution_process_bindings b ON b.process_id = d.process_id
             WHERE d.session_id = $1 ORDER BY d.process_id"
        ).bind(&$session_id).fetch_all(&mut *$tx).await?;
        let receipts = sqlx::query_as::<_, (String, String)>(
            "SELECT r.receipt_key, r.invocation_id FROM execution_provider_receipts r
             JOIN execution_invocations i ON i.invocation_id = r.invocation_id
             WHERE i.session_id = $1 ORDER BY r.receipt_key"
        ).bind(&$session_id).fetch_all(&mut *$tx).await?;
        let replay = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT sequence,event_json,event_bytes FROM execution_replay_events
             WHERE session_id = $1 ORDER BY sequence"
        ).bind(&$session_id).fetch_all(&mut *$tx).await?;
        let replay_cursor: Option<i64> = sqlx::query_scalar(
            "SELECT sequence FROM execution_replay WHERE session_id = $1"
        ).bind(&$session_id).fetch_optional(&mut *$tx).await?;
        let replay_gap = replay_cursor.unwrap_or(0) > 0
            && replay.first().is_none_or(|event| event.0 != 1);
        let financial = financial_snapshot!($backend, $tx, $scope, $session_id, estimated);
        let mut snapshot = serde_json::json!({
            "version": 1,
            "kind": "execution_archive",
            "point_in_time": true,
            "billing_complete": false,
            "payloads_compacted": false,
            "scope": $scope,
            "session_id": $session_id,
            "process_id": $process,
            "state": "closed",
            "recovery_epoch": $epoch,
            "policy_digest": policy,
            "process_binding": binding.map(|(scope, policy_digest)| serde_json::json!({
                "scope": scope, "policy_digest": policy_digest
            })),
            "turns": turns.into_iter().map(|(turn_id, fingerprint, state, outcome_json, accounting_enabled, accounting_coverage)| serde_json::json!({
                "turn_id": turn_id, "fingerprint": fingerprint, "state": state,
                "outcome_json": outcome_json, "accounting_enabled": accounting_enabled != 0,
                "accounting_coverage": accounting_coverage
            })).collect::<Vec<_>>(),
            "resolutions": resolutions.into_iter().map(|(resolution_id, request_json, resolved_at_ms)| serde_json::json!({
                "resolution_id": resolution_id, "request_json": request_json, "resolved_at_ms": resolved_at_ms
            })).collect::<Vec<_>>(),
            "events": events.into_iter().map(|(event_id, event_key, turn_id, kind, created_at_ms, body_json, attempts, delivered_at_ms)| serde_json::json!({
                "event_id": event_id, "event_key": event_key, "turn_id": turn_id, "kind": kind,
                "created_at_ms": created_at_ms, "body_json": body_json,
                "attempts": attempts, "delivered_at_ms": delivered_at_ms
            })).collect::<Vec<_>>(),
            "invocations": invocations.into_iter().map(|(invocation_id, turn_id, request_json, started_at_ms, state, receipt_json, duplicate_receipt_of, finished_at_ms, source_turn_id)| serde_json::json!({
                "invocation_id": invocation_id, "turn_id": turn_id, "request_json": request_json,
                "started_at_ms": started_at_ms, "state": state, "receipt_json": receipt_json,
                "duplicate_receipt_of": duplicate_receipt_of, "finished_at_ms": finished_at_ms,
                "source_turn_id": source_turn_id
            })).collect::<Vec<_>>(),
            "descendants": descendants.into_iter().map(|(process_id,root_turn_id,parent_process_id,registered_at_ms,scope,policy_digest)| serde_json::json!({
                "process_id": process_id,"root_turn_id": root_turn_id,"parent_process_id": parent_process_id,
                "registered_at_ms": registered_at_ms,"scope": scope,"policy_digest": policy_digest
            })).collect::<Vec<_>>(),
            "receipt_bindings": receipts.into_iter().map(|(receipt_key, invocation_id)| serde_json::json!({
                "receipt_key": receipt_key, "invocation_id": invocation_id
            })).collect::<Vec<_>>(),
            "replay_events": replay.into_iter().map(|(sequence,event_json,event_bytes)| serde_json::json!({
                "sequence": sequence,"event_json": event_json,"event_bytes": event_bytes
            })).collect::<Vec<_>>(),
            "replay_cursor": replay_cursor,
            "replay_gap": replay_gap
        });
        snapshot.as_object_mut().expect("archive object").extend(financial);
        snapshot
    }};
}

macro_rules! impl_retention {
    ($backend:ident, $name:ident, $pool:ty, $begin:literal, $lock:literal, $clock:literal) => {
        pub(crate) async fn $name(
            pool: &$pool,
            request: &ExecutionRetentionRequest,
        ) -> Result<ExecutionRetentionResponse, JournalError> {
            request.validate()?;
            let (scope, session_id) = request.identity();
            let id = session_id.to_string();
            let mut tx = pool.begin_with($begin).await?;
            let row = sqlx::query_as::<_, (String, Option<String>, String, i64)>(concat!(
                "SELECT scope, process_id, state, recovery_epoch FROM execution_sessions WHERE session_id = $1", $lock
            )).bind(&id).fetch_optional(&mut *tx).await?.ok_or(JournalError::ExecutionNotFound)?;
            if row.0 != scope { return Err(JournalError::ExecutionNotFound); }
            let existing = sqlx::query_as::<_, ArchiveRow>(
                "SELECT scope, manifest_digest, manifest_json, exported_at_ms, sealed_at_ms, archive_reference, actor
                 FROM execution_archives WHERE session_id = $1 ORDER BY revision DESC LIMIT 1"
            ).bind(&id).fetch_optional(&mut *tx).await?;
            if matches!(request, ExecutionRetentionRequest::Read { .. }) {
                tx.commit().await?;
                return Ok(archive_response(session_id, existing));
            }
            if let ExecutionRetentionRequest::Seal { manifest_digest, archive_reference, actor, .. } = request {
                let sealed = sqlx::query_as::<_, ArchiveRow>(
                    "SELECT scope, manifest_digest, manifest_json, exported_at_ms, sealed_at_ms, archive_reference, actor
                     FROM execution_archives WHERE session_id = $1 AND manifest_digest = $2 AND sealed_at_ms IS NOT NULL"
                ).bind(&id).bind(manifest_digest).fetch_optional(&mut *tx).await?;
                if let Some(sealed) = sealed {
                    if sealed.5.as_ref() != Some(archive_reference) || sealed.6.as_ref() != Some(actor) {
                        return Err(conflict("archive attestation is immutable"));
                    }
                    tx.commit().await?;
                    return Ok(archive_response(session_id, Some(sealed)));
                }
            }
            if row.2 != "closed" { return Err(conflict("only closed sessions can be archived")); }
            let unready: i64 = sqlx::query_scalar(
                "SELECT
                    (SELECT COUNT(*) FROM execution_outbox WHERE session_id = $1 AND delivered_at_ms IS NULL) +
                    (SELECT COUNT(*) FROM execution_invocations WHERE session_id = $1 AND state != 'completed') +
                    (SELECT COUNT(*) FROM execution_turns WHERE session_id = $1 AND state NOT IN ('completed','failed','interrupted','timed_out'))"
            ).bind(&id).fetch_one(&mut *tx).await?;
            if unready != 0 { return Err(conflict("archive requires acknowledged deliveries and terminal known execution evidence")); }
            let snapshot = snapshot!($backend, tx, id, scope, row.1, row.3);
            let json = serde_json::to_string(&snapshot).map_err(|source| JournalError::Serialize { field: "archive manifest", source })?;
            if json.len() > MAX_EXECUTION_ARCHIVE_BYTES {
                return Err(conflict("archive evidence exceeds bounded manifest capacity"));
            }
            let sha256: String = Sha256::digest(json.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
            let sealed_digest: Option<String> = sqlx::query_scalar(
                "SELECT manifest_digest FROM execution_archives WHERE session_id = $1 AND sealed_at_ms IS NOT NULL ORDER BY revision DESC LIMIT 1"
            ).bind(&id).fetch_optional(&mut *tx).await?;
            if sealed_digest.as_ref().is_some_and(|digest| digest != &sha256) {
                return Err(conflict("sealed execution evidence changed"));
            }
            let existing = sqlx::query_as::<_, ArchiveRow>(
                "SELECT scope, manifest_digest, manifest_json, exported_at_ms, sealed_at_ms, archive_reference, actor
                 FROM execution_archives WHERE session_id = $1 AND manifest_digest = $2"
            ).bind(&id).bind(&sha256).fetch_optional(&mut *tx).await?;
            let now: i64 = sqlx::query_scalar($clock).fetch_one(&mut *tx).await?;
            let response = match request {
                ExecutionRetentionRequest::Export { .. } => {
                    if let Some(previous) = existing {
                        if previous.2 != json {
                            return Err(conflict("exported execution evidence changed"));
                        }
                        archive_response(session_id, Some(previous))
                    } else {
                        let revision: i64 = sqlx::query_scalar(
                            "SELECT COALESCE(MAX(revision),0) FROM execution_archives WHERE session_id = $1"
                        ).bind(&id).fetch_one(&mut *tx).await?;
                        let revision = revision.checked_add(1).ok_or_else(|| conflict("archive revision exhausted"))?;
                        sqlx::query(
                            "INSERT INTO execution_archives (session_id,scope,manifest_digest,manifest_json,exported_at_ms,revision)
                             VALUES ($1,$2,$3,$4,$5,$6)"
                        ).bind(&id).bind(scope).bind(&sha256).bind(&json).bind(now).bind(revision).execute(&mut *tx).await?;
                        archive_response(session_id, Some((scope.to_string(), sha256, json, now, None, None, None)))
                    }
                }
                ExecutionRetentionRequest::Seal { manifest_digest, archive_reference, actor, .. } => {
                    let mut previous = existing.ok_or_else(|| conflict("export must commit before archive acknowledgement"))?;
                    if previous.1 != *manifest_digest || previous.1 != sha256 || previous.2 != json {
                        return Err(conflict("archive digest does not match current exported evidence"));
                    }
                    if previous.4.is_some() {
                        if previous.5.as_ref() != Some(archive_reference) || previous.6.as_ref() != Some(actor) {
                            return Err(conflict("archive attestation is immutable"));
                        }
                    } else {
                        sqlx::query(
                            "UPDATE execution_archives SET sealed_at_ms = $1, archive_reference = $2, actor = $3
                             WHERE session_id = $4 AND manifest_digest = $5"
                        ).bind(now).bind(archive_reference).bind(actor).bind(&id).bind(manifest_digest).execute(&mut *tx).await?;
                        previous.4 = Some(now);
                        previous.5 = Some(archive_reference.clone());
                        previous.6 = Some(actor.clone());
                    }
                    archive_response(session_id, Some(previous))
                }
                ExecutionRetentionRequest::Read { .. } => unreachable!("read returns before mutation"),
            };
            tx.commit().await?;
            Ok(response)
        }
    };
}

impl_retention!(
    sqlite,
    execute_retention_sqlite,
    sqlx::SqlitePool,
    "BEGIN IMMEDIATE",
    "",
    "SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 + CAST(substr(strftime('%f', 'now'), 4, 3) AS INTEGER)"
);
impl_retention!(
    postgres,
    execute_retention_postgres,
    sqlx::PgPool,
    "BEGIN",
    " FOR UPDATE",
    "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)"
);

#[cfg(test)]
mod tests;
