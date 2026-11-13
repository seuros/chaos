//! What every session in this OS process is doing, redacted for live monitoring.
//!
//! One bus per OS process, like the global tracer: a resident runs several
//! process tables (serve's own, two per MCP session) and the monitor watches
//! all of them. Events carry names, counts, outcomes and timing only; never
//! prompts, arguments, command lines past the program name, output or file
//! contents. Publishing costs nothing while nobody subscribes.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::EventMsg;
use serde::Serialize;
use tokio::sync::broadcast;

mod check;
mod command;
mod translate;

pub use check::{CheckCounts, CheckKind};

const CAPACITY: usize = 1024;

static BUS: LazyLock<broadcast::Sender<ActivityEvent>> =
    LazyLock::new(|| broadcast::channel(CAPACITY).0);

/// The last `context` and `limits` each turn reported, so a token count that
/// changes neither stays off the bus. Forgotten when the turn ends.
static LAST: LazyLock<Mutex<HashMap<ProcessId, Gauges>>> = LazyLock::new(Default::default);

#[derive(Default)]
struct Gauges {
    context: Option<Activity>,
    limits: Option<Activity>,
}

/// Every session's activity from now on. A slow subscriber lags rather than
/// slowing a session.
pub fn subscribe() -> broadcast::Receiver<ActivityEvent> {
    BUS.subscribe()
}

pub(crate) fn publish(process_id: ProcessId, msg: &EventMsg) {
    if BUS.receiver_count() == 0 {
        return;
    }
    for activity in Activity::from_event(msg) {
        if changed(process_id, &activity) {
            send(process_id, activity);
        }
    }
}

fn changed(process_id: ProcessId, activity: &Activity) -> bool {
    let Ok(mut last) = LAST.lock() else {
        return true;
    };
    let slot = match activity {
        Activity::Context { .. } => &mut last.entry(process_id).or_default().context,
        Activity::Limits { .. } => &mut last.entry(process_id).or_default().limits,
        Activity::TurnEnded { .. } => {
            last.remove(&process_id);
            return true;
        }
        _ => return true,
    };
    if slot.as_ref() == Some(activity) {
        return false;
    }
    *slot = Some(activity.clone());
    true
}

/// The machine's recovery phase changed for this session.
pub(crate) fn machine(
    process_id: ProcessId,
    phase: crate::machine_recovery::Phase,
    causes: Vec<&'static str>,
) {
    if BUS.receiver_count() > 0 {
        send(
            process_id,
            Activity::Machine {
                phase: phase.into(),
                causes: causes.into_iter().filter_map(MachineCause::named).collect(),
            },
        );
    }
}

/// A `needs_you` ask was answered. An ask whose turn ends unanswered is
/// closed by that `turn_ended`, not by this.
pub(crate) fn resolved(process_id: ProcessId, ask_id: String) {
    if BUS.receiver_count() > 0 {
        send(process_id, Activity::Resolved { ask_id });
    }
}

/// The ask id of an MCP server's elicitation, as `needs_you` reports it.
pub(crate) fn elicitation_ask_id(server_name: &str, id: &chaos_ipc::mcp::RequestId) -> String {
    let id = serde_json::to_string(id).unwrap_or_default();
    format!("{server_name}/{id}")
}

/// One model request in flight. `finish` reports its usage; dropping it
/// unfinished (an error, a cancelled turn) reports it failed.
#[must_use = "a model request reports its end when finished or dropped"]
pub(crate) struct ModelRequest {
    process_id: ProcessId,
    request: i64,
    model: String,
    started: std::time::Instant,
    finished: bool,
}

impl ModelRequest {
    pub(crate) fn start(process_id: ProcessId, request: i64, model: &str) -> Self {
        if BUS.receiver_count() > 0 {
            send(
                process_id,
                Activity::ModelStarted {
                    request,
                    model: model.to_string(),
                },
            );
        }
        Self {
            process_id,
            request,
            model: model.to_string(),
            started: std::time::Instant::now(),
            finished: false,
        }
    }

    pub(crate) fn finish(mut self, usage: Option<&chaos_ipc::protocol::TokenUsage>) {
        self.finished = true;
        self.end(CallOutcome::Ok, usage.map(ModelUsage::from));
    }

    fn end(&self, outcome: CallOutcome, usage: Option<ModelUsage>) {
        if BUS.receiver_count() > 0 {
            send(
                self.process_id,
                Activity::ModelEnded {
                    request: self.request,
                    model: self.model.clone(),
                    outcome,
                    duration_ms: translate::millis(self.started.elapsed()),
                    usage,
                },
            );
        }
    }
}

impl Drop for ModelRequest {
    fn drop(&mut self) {
        if !self.finished {
            self.end(CallOutcome::Failed, None);
        }
    }
}

fn send(process_id: ProcessId, activity: Activity) {
    let _ = BUS.send(ActivityEvent {
        process_id,
        at_ms: jiff::Timestamp::now().as_millisecond(),
        activity,
    });
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityEvent {
    pub process_id: ProcessId,
    pub at_ms: i64,
    #[serde(flatten)]
    pub activity: Activity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Activity {
    TurnStarted,
    TurnEnded {
        outcome: TurnOutcome,
    },
    CallStarted {
        call_id: String,
        station: Station,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        check: Option<CheckKind>,
    },
    CallEnded {
        call_id: String,
        station: Station,
        label: String,
        outcome: CallOutcome,
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        check: Option<Check>,
    },
    Context {
        used_tokens: i64,
        window: Option<i64>,
    },
    /// `request` numbers a session's model requests; it pairs the start
    /// with its end even when a workflow samples beside a turn.
    ModelStarted {
        request: i64,
        model: String,
    },
    ModelEnded {
        request: i64,
        model: String,
        outcome: CallOutcome,
        duration_ms: u64,
        usage: Option<ModelUsage>,
    },
    Compacted,
    Limits {
        windows: Vec<LimitWindow>,
    },
    NeedsYou {
        ask_id: String,
        reason: Attention,
    },
    Resolved {
        ask_id: String,
    },
    AgentSpawned {
        child: ProcessId,
        nickname: Option<String>,
        role: Option<String>,
    },
    AgentEnded {
        child: ProcessId,
    },
    /// The host's health as this session's recovery sees it; agents pause
    /// while it is anything but healthy or recovered.
    Machine {
        phase: MachinePhase,
        causes: Vec<MachineCause>,
    },
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MachinePhase {
    Healthy,
    Warning,
    Stabilizing,
    Recovered,
    Unavailable,
    Blocked,
}

impl From<crate::machine_recovery::Phase> for MachinePhase {
    fn from(phase: crate::machine_recovery::Phase) -> Self {
        use crate::machine_recovery::Phase;
        match phase {
            Phase::Healthy => Self::Healthy,
            Phase::Warning => Self::Warning,
            Phase::Stabilizing => Self::Stabilizing,
            Phase::Recovered => Self::Recovered,
            Phase::Unavailable => Self::Unavailable,
            Phase::Blocked => Self::Blocked,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineCause {
    Power,
    Thermal,
    Disk,
}

impl MachineCause {
    fn named(cause: &str) -> Option<Self> {
        match cause {
            "power" => Some(Self::Power),
            "thermal" => Some(Self::Thermal),
            "disk" => Some(Self::Disk),
            _ => None,
        }
    }
}

/// A check that ran: its kind and, when the runner printed them, its counts.
/// Counts can disagree with the outcome when a pipe hid the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Check {
    pub kind: CheckKind,
    pub counts: Option<CheckCounts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModelUsage {
    pub input: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub reasoning: i64,
}

impl From<&chaos_ipc::protocol::TokenUsage> for ModelUsage {
    fn from(usage: &chaos_ipc::protocol::TokenUsage) -> Self {
        Self {
            input: usage.input_tokens,
            cache_read: usage.cache_read_input(),
            cache_write: usage.cache_creation_input_tokens,
            output: usage.output_tokens,
            reasoning: usage.reasoning_output_tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimitWindow {
    pub used_percent: u8,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Station {
    /// Chaos's own tools: checkpoints, recovery, tool loading.
    Kernel,
    Shell,
    Files,
    Network,
    Mcp,
    Web,
    Edit,
    Hooks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    Ok,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    CommandApproval,
    PatchApproval,
    Permissions,
    UserInput,
    Elicitation,
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
