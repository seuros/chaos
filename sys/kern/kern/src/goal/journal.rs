use super::*;
use chaos_ipc::protocol::{GoalCheckpointItem, RolloutItem};
use chaos_mcp_protocol::goals::GoalLifecycleEvent;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct Journal {
    pub(super) goal: Option<Snapshot>,
    pub(super) verdict: Option<VerdictRecord>,
    pub(super) server: Option<String>,
    endpoint: Option<String>,
    connections: HashMap<(String, String), u64>,
    pub(super) interrupted: bool,
    pub(super) evaluating: Option<u64>,
    pending: Option<(GoalCheckpointItem, bool)>,
}

impl Journal {
    pub(super) fn bind(
        &mut self,
        server: &str,
        endpoint: &str,
        connection: u64,
        restore: bool,
    ) -> Result<(), String> {
        if self.server.as_deref().is_some_and(|s| s != server)
            || self.endpoint.as_deref().is_some_and(|e| e != endpoint)
        {
            return Err("goal belongs to another driver route".into());
        }
        let key = (server.into(), endpoint.into());
        match self.connections.get(&key) {
            Some(current) if *current > connection => return Err("retired goal connection".into()),
            Some(current) if *current == connection => {}
            _ if restore => {
                self.connections.insert(key, connection);
                self.interrupted |= self
                    .goal
                    .as_ref()
                    .is_some_and(|g| matches!(g.status, Status::Active | Status::Checking));
            }
            _ => return Err("restore goals before mutation".into()),
        }
        Ok(())
    }

    pub(super) fn validate(
        &self,
        gate: &GoalGate,
        server: &str,
        expected: u64,
        next: &Snapshot,
    ) -> Result<(), String> {
        let revision = self.goal.as_ref().map_or(0, |g| g.revision);
        if expected != revision
            || expected.checked_add(1) != Some(next.revision)
            || !next.validate()
        {
            return Err("stale or invalid goal revision".into());
        }
        let context = gate.context();
        let permit = gate
            .permit
            .as_ref()
            .filter(|p| p.server == server && p.turn_id == context.turn_id);
        let previous = self.goal.as_ref();
        let starting = previous.is_none_or(|g| g.id != next.id);
        if starting {
            if !context.eligible
                || next.turn_id != context.turn_id
                || previous.is_some_and(|g| g.turn_id == next.turn_id)
                || next.status != Status::Active
                || next.checks != 0
                || next.attempt_id.is_some()
                || next.evidence_digest.is_some()
                || next.verdict_ref.is_some()
                || !permit.is_some_and(|p| {
                    p.tool == "start_goal"
                        && p.arguments
                            == json!({"objective":next.objective,"criteria":next.criteria})
                })
            {
                return Err("goal start not authorized for this turn".into());
            }
            return Ok(());
        }
        let old = previous.ok_or("missing prior goal")?;
        if old.turn_id != next.turn_id
            || old.objective != next.objective
            || old.criteria != next.criteria
        {
            return Err("goal definition is immutable".into());
        }
        let same_attempt = next.checks == old.checks
            && next.attempt_id == old.attempt_id
            && next.evidence_digest == old.evidence_digest
            && next.verdict_ref == old.verdict_ref;
        let resumable = matches!(old.status, Status::Active | Status::Checking)
            || (old.status == Status::Complete && self.interrupted);
        if next.status == Status::Paused
            && old
                .status
                .accepts(GoalLifecycleEvent::Pause(same_attempt && resumable))
        {
            return Ok(());
        }
        if !context.eligible || next.turn_id != context.turn_id || self.interrupted {
            return Err("goal turn is fenced".into());
        }
        if next.status == Status::Active
            && old.status.accepts(GoalLifecycleEvent::Reopen(
                same_attempt && old.evidence_digest.as_deref() != Some(&context.evidence_digest),
            ))
        {
            return Ok(());
        }
        if next.status == Status::Checking
            && old.status.accepts(GoalLifecycleEvent::Check(
                old.checks < MAX_CHECKS
                    && next.checks == old.checks + 1
                    && next.attempt_id.is_some()
                    && next.attempt_id != old.attempt_id
                    && context.has_evidence
                    && next.evidence_digest.as_deref() == Some(&context.evidence_digest)
                    && next.evidence_digest != old.evidence_digest
                    && next.verdict_ref.is_none()
                    && permit.is_some_and(|p| p.tool == "check_goal"),
            ))
        {
            return Ok(());
        }
        if next.checks == old.checks
            && next.attempt_id == old.attempt_id
            && next.evidence_digest == old.evidence_digest
            && next.evidence_digest.as_deref() == Some(&context.evidence_digest)
            && let Some(verdict) = self.verdict.as_ref().filter(|v| v.revision == old.revision)
            && old.attempt_id.as_ref() == Some(&verdict.attempt_id)
            && old.evidence_digest.as_ref() == Some(&verdict.evidence_digest)
            && next.verdict_ref.as_ref() == Some(&verdict.reference)
        {
            let event = match &verdict.evaluation {
                Evaluation::Complete => GoalLifecycleEvent::Pass(next.status == Status::Complete),
                Evaluation::Incomplete { .. } if old.checks < MAX_CHECKS => {
                    GoalLifecycleEvent::Retry(next.status == Status::Active)
                }
                _ => GoalLifecycleEvent::Stop(next.status == Status::Paused),
            };
            if old.status.accepts(event) {
                return Ok(());
            }
        }
        Err("unapproved goal transition".into())
    }

    pub(super) async fn commit(
        &mut self,
        sess: &Session,
        server: &str,
        endpoint: &str,
        goal: Snapshot,
        verdict: Option<VerdictRecord>,
    ) -> Result<(), String> {
        self.pending = Some((
            GoalCheckpointItem {
                conversation_id: sess.conversation_id,
                server: server.into(),
                endpoint: endpoint.into(),
                version: VERSION,
                snapshot: json!(goal),
                verdict: verdict.map(|v| json!(v)),
            },
            false,
        ));
        self.reconcile(sess).await
    }

    pub(super) async fn reconcile(&mut self, sess: &Session) -> Result<(), String> {
        let Some((item, queued)) = self.pending.as_mut() else {
            return Ok(());
        };
        let recorder = sess
            .services
            .rollout
            .lock()
            .await
            .clone()
            .ok_or("durable goal storage unavailable")?;
        if !*queued {
            recorder
                .record_items(&[RolloutItem::GoalCheckpoint(item.clone())])
                .await
                .map_err(|_| "goal journal admission failed")?;
            *queued = true;
        }
        recorder
            .confirm()
            .await
            .map_err(|_| "goal journal commit unconfirmed")?;
        let item = self.pending.take().ok_or("missing pending checkpoint")?.0;
        self.apply(item)?;
        Ok(())
    }

    fn apply(&mut self, item: GoalCheckpointItem) -> Result<(), String> {
        if item.version != VERSION {
            return Err("unsupported stored goal version".into());
        }
        let goal: Snapshot =
            serde_json::from_value(item.snapshot).map_err(|_| "invalid stored goal")?;
        if !goal.validate() {
            return Err("invalid stored goal bounds".into());
        }
        self.interrupted &= matches!(goal.status, Status::Active | Status::Checking);
        self.server = Some(item.server);
        self.endpoint = Some(item.endpoint);
        self.goal = Some(goal);
        self.verdict = item
            .verdict
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| "invalid stored verdict")?;
        Ok(())
    }

    pub(super) fn restore(
        &mut self,
        conversation: chaos_ipc::ProcessId,
        items: &[RolloutItem],
    ) -> Result<(), String> {
        let mut later_evidence = evidence::Evidence::default();
        let mut changed = false;
        let mut closed = false;
        for item in items {
            match item {
                RolloutItem::GoalCheckpoint(item) if item.conversation_id == conversation => {
                    self.apply(item.clone())?;
                    later_evidence = Default::default();
                    changed = false;
                    closed = false;
                }
                RolloutItem::EventMsg(EventMsg::TurnComplete(_)) | RolloutItem::TurnContext(_) => {
                    closed = true
                }
                RolloutItem::ResponseItem(ResponseItem::Message { role, .. }) if role == "user" => {
                    closed = true
                }
                RolloutItem::ResponseItem(item)
                    if !closed
                        && self
                            .goal
                            .as_ref()
                            .is_some_and(|g| g.status == Status::Complete) =>
                {
                    changed |= later_evidence.record(item);
                }
                _ => {}
            }
        }
        self.interrupted = changed
            || self
                .goal
                .as_ref()
                .is_some_and(|g| matches!(g.status, Status::Active | Status::Checking));
        Ok(())
    }
}
