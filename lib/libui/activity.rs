//! Live, per-process activity, independent of transcript replay and estimated tokens.
//!
//! Receiving runtime traffic is not evidence of model progress. Only observed work
//! renews `last_activity`; no timer, status poll, or other agent can renew it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{AgentStatus, Event, EventMsg, Op};

pub(crate) mod captions;
mod events;

pub const QUIET_AFTER: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Working,
    Tools,
    WaitingAgents,
    NeedsInput,
    Reconnecting,
    Failed,
    Interrupted,
    Closed,
    Disconnected,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Starting => "Starting",
            Self::Working => "Working",
            Self::Tools => "Tools",
            Self::WaitingAgents => "Waiting on agents",
            Self::NeedsInput => "Needs input",
            Self::Reconnecting => "Reconnecting",
            Self::Failed => "Error",
            Self::Interrupted => "Interrupted",
            Self::Closed => "Closed",
            Self::Disconnected => "Disconnected",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub phase: Phase,
    pub last_activity: Option<Instant>,
    /// Includes synthetic progress traffic. Never used to clear a quiet warning.
    pub last_runtime_event: Option<Instant>,
    /// Start of the current expectation of work (e.g. after an operator reply).
    /// This gives resumed work a grace period without inventing model activity.
    pub expecting_since: Option<Instant>,
}

impl Activity {
    pub fn age(&self, now: Instant) -> Duration {
        self.last_activity
            .map(|at| now.saturating_duration_since(at))
            .unwrap_or_default()
    }

    pub fn is_quiet(&self, now: Instant) -> bool {
        self.is_active() && self.quiet_for(now) >= QUIET_AFTER
    }

    fn quiet_for(&self, now: Instant) -> Duration {
        self.last_activity
            .max(self.expecting_since)
            .map(|at| now.saturating_duration_since(at))
            .unwrap_or_default()
    }

    pub fn is_active(&self) -> bool {
        matches!(self.phase, Phase::Starting | Phase::Working | Phase::Tools)
    }

    pub fn label(&self, now: Instant) -> String {
        if self.is_quiet(now) {
            let seconds = self.quiet_for(now).as_secs();
            if self.phase == Phase::Tools {
                format!("Tools · no activity {seconds}s")
            } else {
                format!("No activity · {seconds}s")
            }
        } else {
            self.phase.label().to_string()
        }
    }

    pub fn description(&self, now: Instant) -> String {
        let mut text = self.phase.label().to_string();
        if self.last_activity.is_some() {
            text.push_str(&format!(
                " · last activity {}s ago",
                self.age(now).as_secs()
            ));
        }
        if let Some(at) = self.last_runtime_event {
            text.push_str(&format!(
                " · runtime event {}s ago",
                now.saturating_duration_since(at).as_secs()
            ));
        }
        text
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub selected: Activity,
    /// Includes all other tracked processes, including the parent when viewing a child.
    pub others: Vec<(ProcessId, Activity)>,
    pub animations: bool,
}

#[derive(Default)]
struct ProcessActivity {
    activity: Activity,
    turn: Option<String>,
    tools: HashSet<(&'static str, String)>,
    waits: HashSet<String>,
    inputs: HashSet<String>,
}

impl ProcessActivity {
    fn refresh_phase(&mut self) {
        self.activity.phase = if !self.inputs.is_empty() {
            Phase::NeedsInput
        } else if !self.waits.is_empty() {
            Phase::WaitingAgents
        } else if !self.tools.is_empty() {
            Phase::Tools
        } else {
            Phase::Working
        };
    }

    fn finish(&mut self, phase: Phase) {
        self.turn = None;
        self.tools.clear();
        self.waits.clear();
        self.inputs.clear();
        self.activity.phase = phase;
    }
}

#[derive(Default)]
pub struct Tracker {
    processes: HashMap<ProcessId, ProcessActivity>,
}

impl Tracker {
    pub fn clear(&mut self) {
        self.processes.clear();
    }

    pub fn get(&self, process_id: ProcessId) -> Activity {
        self.processes
            .get(&process_id)
            .map(|state| state.activity.clone())
            .unwrap_or_default()
    }

    pub fn snapshot(&self, selected: Option<ProcessId>, animations: bool) -> Snapshot {
        let mut others: Vec<_> = self
            .processes
            .iter()
            .filter(|(id, _)| Some(**id) != selected)
            .map(|(id, state)| (*id, state.activity.clone()))
            .collect();
        others.sort_by_cached_key(|(id, _)| id.to_string());
        Snapshot {
            selected: selected.map(|id| self.get(id)).unwrap_or_default(),
            others,
            animations,
        }
    }

    pub fn disconnected(&mut self, process_id: ProcessId) {
        let state = self.processes.entry(process_id).or_default();
        // A normal shutdown followed by EOF must remain "Closed".
        if state.activity.phase != Phase::Closed {
            state.finish(Phase::Disconnected);
        }
    }

    pub fn closed(&mut self, process_id: ProcessId) {
        self.processes
            .entry(process_id)
            .or_default()
            .finish(Phase::Closed);
    }

    /// Remote status reports can establish or end a lifecycle, but repeated
    /// "Running" reports must not make a silent child look busy.
    fn reported_status(&mut self, process_id: ProcessId, status: &AgentStatus, now: Instant) {
        let state = self.processes.entry(process_id).or_default();
        if state.activity.last_runtime_event.is_some() {
            return;
        }
        match status {
            AgentStatus::Completed(_) => state.finish(Phase::Idle),
            AgentStatus::Interrupted => state.finish(Phase::Interrupted),
            AgentStatus::Errored(_) => state.finish(Phase::Failed),
            AgentStatus::Shutdown => state.finish(Phase::Closed),
            AgentStatus::NotFound => state.finish(Phase::Disconnected),
            AgentStatus::PendingInit | AgentStatus::Running => {
                state.activity.phase = if *status == AgentStatus::PendingInit {
                    Phase::Starting
                } else {
                    Phase::Working
                };
                state.activity.expecting_since.get_or_insert(now);
            }
        }
    }

    /// Only call at live ingress, never while displaying a saved transcript.
    pub fn observe(&mut self, process_id: ProcessId, event: &Event, now: Instant) {
        self.observe_local(process_id, event, now);
        match &event.msg {
            EventMsg::CollabAgentSpawnEnd(ev) => {
                if let Some(id) = ev.new_process_id {
                    self.reported_status(id, &ev.status, now);
                }
            }
            // Close reports the PRE-close status even if shutdown fails. Only
            // the child's own ShutdownComplete establishes a live child closed.
            EventMsg::CollabCloseEnd(ev) => {
                self.reported_status(ev.receiver_process_id, &ev.status, now);
            }
            EventMsg::CollabResumeEnd(ev) => {
                self.reported_status(ev.receiver_process_id, &ev.status, now)
            }
            // Waiting results may be delayed relative to the child's own event
            // stream. Do not overwrite independently observed child state.
            _ => {}
        }
    }

    /// Resolving one prompt must not hide other concurrent prompts.
    pub fn note_op(&mut self, process_id: ProcessId, op: &Op, now: Instant) {
        if let Op::UserInputAnswer { id, .. } = op {
            if let Some(state) = self.processes.get_mut(&process_id) {
                let prefix = format!("input:{id}:");
                let key = state
                    .inputs
                    .iter()
                    .find(|key| key.starts_with(&prefix))
                    .cloned();
                if let Some(key) = key {
                    state.inputs.remove(&key);
                    if state.turn.is_some() {
                        state.refresh_phase();
                        state.activity.expecting_since = Some(now);
                    }
                }
            }
            return;
        }
        let key = match op {
            Op::ExecApproval { id, .. } => format!("exec:{id}"),
            Op::PatchApproval { id, .. } => format!("patch:{id}"),
            Op::RequestPermissionsResponse { id, .. } => format!("permissions:{id}"),
            Op::ResolveElicitation {
                server_name,
                request_id,
                ..
            } => {
                format!("elicitation:{server_name}:{request_id}")
            }
            _ => return,
        };
        if let Some(state) = self.processes.get_mut(&process_id)
            && state.inputs.remove(&key)
            && state.turn.is_some()
        {
            state.refresh_phase();
            state.activity.expecting_since = Some(now);
        }
    }
}

#[cfg(test)]
mod tests;
