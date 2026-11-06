//! Saga state machine for the submit path: the persisted progress record,
//! the in-flight run, and the driver that walks forward or compensates.

use std::time::Duration;

use dedup_machines::Claim;
use dedup_machines::ClaimId;
use dedup_machines::Dedup;
use saga_machines::Action;
use saga_machines::ActionKey;
use saga_machines::Kind;
use saga_machines::Outcome;
use saga_machines::Progress;
use saga_machines::Saga;
use saga_machines::SagaId;
use saga_machines::Snapshot;
use serde::Deserialize;
use serde::Serialize;

use super::PollWiring;
use super::SubmitEffects;
use super::SubmitReceipt;
use crate::job::CronJob;
use crate::spool_store::SpoolRow;

const STEPS: usize = 5;
const STEP_INSERT_QUEUED: usize = 0;
pub(super) const STEP_SUBMIT: usize = 1;
pub(super) const STEP_INSERT_SUBMITTED: usize = 2;
const STEP_CREATE_POLL_ROW: usize = 3;
const STEP_RETIRE_POLL_ROWS: usize = 4;
const LEDGER_SLOTS: usize = STEPS * 2;
const LEDGER_TTL: Duration = Duration::from_secs(3600);
const COMPENSATION_RETRIES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Direction {
    Forward,
    Compensating,
}

/// Persisted shape of `spool_jobs.saga_json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SagaState {
    pub(super) direction: Direction,
    pub(super) step: usize,
    pub(super) attempt: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) poll: Option<PollWiring>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) poll_cron_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

impl SagaState {
    fn from_snapshot(snapshot: Snapshot, run: &SubmitRun) -> Option<Self> {
        let (direction, step) = match snapshot.progress {
            Progress::Forward(step) => (Direction::Forward, step),
            Progress::Compensating(step) => (Direction::Compensating, step),
            _ => return None,
        };
        Some(Self {
            direction,
            step,
            attempt: snapshot.attempt,
            poll: run.poll.clone(),
            poll_cron_id: run.poll_cron_id.clone(),
            error: run.error.clone(),
        })
    }

    fn snapshot(&self, id: SagaId) -> Snapshot {
        let progress = match self.direction {
            Direction::Forward => Progress::Forward(self.step),
            Direction::Compensating => Progress::Compensating(self.step),
        };
        Snapshot {
            id: id.raw(),
            progress,
            attempt: self.attempt,
        }
    }
}

pub(super) struct SubmitRun {
    saga: Saga<STEPS>,
    ledger: Dedup<ActionKey, LEDGER_SLOTS>,
    origin: tokio::time::Instant,
    poll: Option<PollWiring>,
    poll_cron_id: Option<String>,
    poll_job: Option<CronJob>,
    replaced_poll_rows: u64,
    batch_id: Option<String>,
    error: Option<String>,
}

impl SubmitRun {
    pub(super) fn fresh(manifest_id: &str, poll: Option<PollWiring>) -> Self {
        Self::with_saga(
            Saga::new(SagaId::from_bytes(manifest_id.as_bytes()).raw()),
            poll,
            None,
        )
    }

    pub(super) fn restored(row: &SpoolRow) -> Result<Self, String> {
        let manifest_id = &row.manifest_id;
        let saga_json = row
            .saga_json
            .as_deref()
            .ok_or_else(|| format!("spool row {manifest_id} has no pending saga"))?;
        let state: SagaState = serde_json::from_str(saga_json)
            .map_err(|e| format!("decode saga for {manifest_id}: {e}"))?;
        let id = SagaId::from_bytes(manifest_id.as_bytes());
        let saga = Saga::restore(state.snapshot(id))
            .ok_or_else(|| format!("saga for {manifest_id} does not fit {STEPS} steps"))?;
        let mut run = Self::with_saga(saga, state.poll, row.batch_id.clone());
        run.poll_cron_id = state.poll_cron_id;
        run.error = state.error;
        Ok(run)
    }

    fn with_saga(saga: Saga<STEPS>, poll: Option<PollWiring>, batch_id: Option<String>) -> Self {
        Self {
            saga,
            ledger: Dedup::new(),
            origin: tokio::time::Instant::now(),
            poll,
            poll_cron_id: None,
            poll_job: None,
            replaced_poll_rows: 0,
            batch_id,
            error: None,
        }
    }

    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

pub(super) async fn drive<E: SubmitEffects>(
    effects: &mut E,
    mut run: SubmitRun,
) -> Result<SubmitReceipt, String> {
    let mut compensation_failures = 0;
    while let Some(action) = run.saga.next_action() {
        let now = run.now();
        let ticket = match action.claim(&mut run.ledger, now, LEDGER_TTL) {
            Claim::Acquired(id) => Ticket::Fresh(id),
            Claim::Completed(_) => Ticket::Done,
            _ => Ticket::Refused,
        };
        let outcome = match ticket {
            Ticket::Done => Outcome::Success,
            Ticket::Refused => {
                return Err(format!(
                    "submit saga ledger refused step {} ({:?})",
                    action.step(),
                    action.kind()
                ));
            }
            Ticket::Fresh(claim) => match execute(effects, &mut run, action).await {
                Ok(()) => {
                    let _ = run.ledger.complete(claim, run.now());
                    Outcome::Success
                }
                Err(error) => {
                    let _ = run.ledger.abandon(claim);
                    if action.kind() == Kind::Forward {
                        run.error = Some(error);
                    } else {
                        compensation_failures += 1;
                        if compensation_failures > COMPENSATION_RETRIES {
                            persist(effects, &run).await?;
                            return Err(format!(
                                "submit saga compensation stalled at step {}: {error}",
                                action.step()
                            ));
                        }
                    }
                    Outcome::Failure
                }
            },
        };
        if !run.saga.resolve(action, outcome) {
            return Err("submit saga rejected its own attempt".to_string());
        }
        persist(effects, &run).await?;
    }
    match run.saga.progress() {
        Progress::Complete => {
            let batch_id = run
                .batch_id
                .ok_or_else(|| "submit saga completed without a batch id".to_string())?;
            Ok(SubmitReceipt {
                batch_id,
                poll_job: run.poll_job,
                replaced_poll_rows: run.replaced_poll_rows,
            })
        }
        _ => Err(run
            .error
            .unwrap_or_else(|| "submit saga aborted".to_string())),
    }
}

enum Ticket {
    Fresh(ClaimId),
    Done,
    Refused,
}

async fn persist<E: SubmitEffects>(effects: &mut E, run: &SubmitRun) -> Result<(), String> {
    let state = SagaState::from_snapshot(run.saga.snapshot(), run);
    effects
        .persist(state.as_ref(), run.batch_id.as_deref())
        .await
}

async fn execute<E: SubmitEffects>(
    effects: &mut E,
    run: &mut SubmitRun,
    action: Action,
) -> Result<(), String> {
    match (action.kind(), action.step()) {
        (Kind::Forward, STEP_INSERT_QUEUED) => effects.insert_queued().await,
        (Kind::Forward, STEP_SUBMIT) => {
            if run.batch_id.is_none() {
                run.batch_id = Some(effects.submit().await?);
            }
            Ok(())
        }
        (Kind::Forward, STEP_INSERT_SUBMITTED) => {
            let batch_id = run
                .batch_id
                .as_deref()
                .ok_or_else(|| "submit saga lost its batch id".to_string())?;
            effects.insert_submitted(batch_id).await
        }
        (Kind::Forward, STEP_CREATE_POLL_ROW) => {
            if let Some(poll) = &run.poll {
                let job = effects.create_poll_row(poll).await?;
                run.poll_cron_id = Some(job.id.clone());
                run.poll_job = Some(job);
            }
            Ok(())
        }
        (Kind::Forward, STEP_RETIRE_POLL_ROWS) => {
            if let Some(keep) = &run.poll_cron_id {
                run.replaced_poll_rows = effects.retire_poll_rows(keep).await?;
            }
            Ok(())
        }
        (Kind::Compensate, STEP_INSERT_QUEUED | STEP_INSERT_SUBMITTED) => {
            let error = run.error.as_deref().unwrap_or("submit saga aborted");
            effects.mark_failed(error).await
        }
        (Kind::Compensate, STEP_SUBMIT) => match run.batch_id.as_deref() {
            Some(batch_id) => effects.cancel(batch_id).await,
            None => Ok(()),
        },
        (Kind::Compensate, STEP_CREATE_POLL_ROW) => match run.poll_cron_id.as_deref() {
            Some(job_id) => effects.delete_poll_row(job_id).await,
            None => Ok(()),
        },
        _ => Ok(()),
    }
}
