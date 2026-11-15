//! Sessions still open, and what a resident that connects late must be told
//! about them: health and MCP servers are sent only when they change.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::EventMsg;

use super::{Activity, ActivityEvent, McpState};

/// Sessions open in this process, so a resident that connects late can be
/// told about the idle ones too.
pub(super) static OPEN: LazyLock<Mutex<HashMap<ProcessId, Open>>> = LazyLock::new(Default::default);

/// What a late resident needs to know about a session already open.
pub(super) struct Open {
    pub model: String,
    pub at_ms: i64,
    /// The machine health it last reported; health is sent only on change.
    pub machine: Option<Activity>,
    /// Its MCP servers and the state each last reported.
    pub mcp: BTreeMap<String, McpState>,
}

pub(super) fn track_open(process_id: ProcessId, msg: &EventMsg) {
    let Ok(mut open) = OPEN.lock() else { return };
    match msg {
        EventMsg::SessionConfigured(ev) => {
            open.insert(
                process_id,
                Open {
                    model: ev.model.clone(),
                    at_ms: jiff::Timestamp::now().as_millisecond(),
                    machine: None,
                    mcp: BTreeMap::new(),
                },
            );
        }
        EventMsg::McpStartupUpdate(ev) => {
            if let Some(session) = open.get_mut(&process_id) {
                session.mcp.insert(ev.server.clone(), (&ev.status).into());
            }
        }
        EventMsg::ShutdownComplete => {
            open.remove(&process_id);
        }
        _ => {}
    }
}

/// One `session_opened` per session still open, as when each began.
pub fn open_sessions() -> Vec<ActivityEvent> {
    let Ok(open) = OPEN.lock() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for (process_id, session) in open.iter() {
        events.push(ActivityEvent {
            process_id: *process_id,
            at_ms: session.at_ms,
            activity: Activity::SessionOpened {
                model: session.model.clone(),
            },
        });
        if let Some(machine) = &session.machine {
            events.push(ActivityEvent {
                process_id: *process_id,
                at_ms: session.at_ms,
                activity: machine.clone(),
            });
        }
        for (server, state) in &session.mcp {
            events.push(ActivityEvent {
                process_id: *process_id,
                at_ms: session.at_ms,
                activity: Activity::McpServer {
                    server: server.clone(),
                    state: *state,
                },
            });
        }
    }
    events
}

/// The machine health this session last reported.
pub(super) fn remember_machine(process_id: ProcessId, activity: &Activity) {
    if let Ok(mut open) = OPEN.lock()
        && let Some(session) = open.get_mut(&process_id)
    {
        session.machine = Some(activity.clone());
    }
}
