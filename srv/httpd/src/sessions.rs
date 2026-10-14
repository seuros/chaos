//! Experimental daemon-owned sessions. HTTP requests never own execution or
//! consume the kernel event queue. A single supervisor owns each queue.
pub(crate) mod http;
mod runtime;
mod stream;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::{Event, EventMsg};
use rama::http::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;
use uuid::Uuid;

use crate::protocol::{TokenUsageEntry, TokenUsageResponse};

const MAX_TURNS: usize = 256;
const MAX_EVENTS: usize = 256;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_REPLAY_BYTES: usize = 1024 * 1024;
const MAX_RESULT_BYTES: usize = 16 * 1024;
const CLEANUP_GRACE: Duration = Duration::from_secs(30);

pub(crate) struct SessionManager {
    pub daemon_id: String,
    pub ttl: Duration,
    ingress: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
    limit: usize,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl SessionManager {
    pub(crate) fn new(limit: usize, ttl: Duration) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            daemon_id: Uuid::new_v4().to_string(),
            ttl,
            ingress: Arc::new(Semaphore::new(16)),
            shutdown,
            limit,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn reserve(
        &self,
        resume: Option<ProcessId>,
    ) -> Result<(Arc<Session>, mpsc::Receiver<Command>), ApiError> {
        let mut sessions = self.sessions.lock().unwrap();
        self.ensure_running()?;
        let resume = resume.map(|id| id.to_string());
        if let Some(resume) = &resume {
            for session in sessions.values() {
                let data = session.data.lock().unwrap();
                if data.process_id.as_ref() == Some(resume)
                    && !matches!(data.status, SessionStatus::Closed | SessionStatus::Failed)
                {
                    return Err(ApiError::conflict(
                        "process is already owned by a retained session",
                    ));
                }
            }
        }
        if sessions.len() >= self.limit {
            return Err(ApiError::capacity("session limit reached"));
        }
        let (tx, rx) = mpsc::channel(32);
        let (changed, _) = watch::channel(());
        let mut data = SessionData::new();
        data.process_id = resume;
        let session = Arc::new(Session {
            id: Uuid::new_v4().to_string(),
            commands: tx,
            data: Mutex::new(data),
            changed,
        });
        sessions.insert(session.id.clone(), session.clone());
        Ok((session, rx))
    }

    fn ensure_running(&self) -> Result<(), ApiError> {
        if *self.shutdown.borrow() {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "daemon is shutting down",
            ));
        }
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Arc<Session>, ApiError> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(ApiError::new(StatusCode::NOT_FOUND, "session not found"))
    }

    fn snapshots(&self) -> Vec<SessionSnapshot> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .map(|session| session.snapshot())
            .collect()
    }

    fn forget(&self, id: &str) -> Result<(), ApiError> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get(id)
            .ok_or(ApiError::new(StatusCode::NOT_FOUND, "session not found"))?;
        let data = session.data.lock().unwrap();
        if !matches!(data.status, SessionStatus::Closed | SessionStatus::Failed) {
            return Err(ApiError::conflict("session has not stopped"));
        }
        drop(data);
        sessions.remove(id);
        Ok(())
    }

    pub(crate) fn close_all(&self) {
        // Wake SSE subscribers even if containment failed or the supervisor
        // died. Stream closure is not proof that execution has stopped.
        self.shutdown.send_replace(true);
        for session in self.sessions.lock().unwrap().values() {
            let _ = session.close();
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    /// Explicitly restore saved history; never automatically retry an old turn.
    pub resume_process_id: Option<ProcessId>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnRequest {
    pub request: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl TurnRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.request.trim().is_empty()
            || self
                .model
                .as_ref()
                .is_some_and(|model| model.trim().is_empty())
        {
            return Err(ApiError::bad_request("request and model must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SteerRequest {
    pub request: String,
}

/// A host notice for the active turn: unlike steering, it is held in the
/// session's single notice slot until the next delivery boundary, where its
/// expiry is checked. Posting a new `id` replaces it. Re-posting an id the
/// session has already seen (posted or delivered) is a no-op, so a retried or
/// reordered request cannot re-deliver an old notice or displace a newer one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoticeRequest {
    pub id: String,
    pub text: String,
    /// Unix seconds after which the notice must not be delivered.
    #[serde(default)]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SessionStatus {
    Starting,
    Idle,
    Running,
    Closing,
    Closed,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TurnStatus {
    Accepted,
    Running,
    Completed,
    Failed,
    WaitingBackground,
    Interrupting,
    Interrupted,
    TimedOut,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionSnapshot {
    pub session_id: String,
    status: SessionStatus,
    process_id: Option<String>,
    active_turn_id: Option<String>,
    last_event_id: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TurnSnapshot {
    pub turn_id: String,
    status: TurnStatus,
    result: Option<String>,
    result_truncated: bool,
    error: Option<String>,
    usage: Option<TokenUsageResponse>,
}

struct TurnRecord {
    fingerprint: [u8; 32],
    // Cleared as soon as the supervisor consumes it, rather than retaining
    // potentially megabyte-sized prompts with every deduplication tombstone.
    request: Option<TurnRequest>,
    snapshot: TurnSnapshot,
}

struct ActiveTurn {
    id: String,
    deadline: Instant,
    completion_outcome: Option<TurnStatus>,
    // Held until a matching terminal event or confirmed session shutdown.
    _permit: OwnedSemaphorePermit,
}

pub(crate) struct Session {
    id: String,
    commands: mpsc::Sender<Command>,
    data: Mutex<SessionData>,
    changed: watch::Sender<()>,
}

struct SessionData {
    status: SessionStatus,
    process_id: Option<String>,
    turns: BTreeMap<String, TurnRecord>,
    active: Option<ActiveTurn>,
    events: VecDeque<ReplayEvent>,
    replay_bytes: usize,
    sequence: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ReplayEvent {
    sequence: u64,
    event: serde_json::Value,
    #[serde(skip)]
    bytes: usize,
}

#[derive(Debug, Serialize)]
struct EventBatch {
    events: Vec<ReplayEvent>,
    gap: bool,
    next_cursor: u64,
}

/// Notice ids travel in a DELETE query string, so they are restricted to the
/// URL-safe unreserved alphabet and need no percent-encoding.
pub(crate) fn valid_notice_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

enum NoticeChange {
    Post(chaos_kern::host_notice::HostNotice),
    /// Withdraw, optionally only if this id is the one pending.
    Withdraw(Option<String>),
}

enum Command {
    Turn(String),
    Steer {
        turn_id: String,
        text: String,
        reply: oneshot::Sender<Result<(), ApiError>>,
    },
    Interrupt(String),
    Notice {
        turn_id: String,
        change: NoticeChange,
        reply: oneshot::Sender<Result<bool, ApiError>>,
    },
    Close,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    code: StatusCode,
    message: &'static str,
}

impl ApiError {
    fn new(code: StatusCode, message: &'static str) -> Self {
        Self { code, message }
    }
    fn bad_request(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn conflict(message: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    fn capacity(message: &'static str) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, message)
    }
}

impl From<crate::ingress::BodyError> for ApiError {
    fn from(error: crate::ingress::BodyError) -> Self {
        Self::new(error.status(), error.message())
    }
}

impl Session {
    fn notify(&self) {
        self.changed.send_replace(());
    }

    #[cfg(test)]
    fn observe(&self, event: &Event) {
        self.observe_with_activity(event, true);
    }

    fn observe_with_activity(&self, event: &Event, quiescent: bool) {
        self.data.lock().unwrap().observe(event, quiescent);
        self.notify();
    }

    fn snapshot(&self) -> SessionSnapshot {
        let data = self.data.lock().unwrap();
        SessionSnapshot {
            session_id: self.id.clone(),
            status: data.status,
            process_id: data.process_id.clone(),
            active_turn_id: data.active.as_ref().map(|turn| turn.id.clone()),
            last_event_id: data.sequence,
        }
    }

    fn turn(&self, id: &str) -> Result<TurnSnapshot, ApiError> {
        self.data
            .lock()
            .unwrap()
            .turns
            .get(id)
            .map(|turn| turn.snapshot.clone())
            .ok_or(ApiError::new(StatusCode::NOT_FOUND, "turn not found"))
    }

    fn submit(
        &self,
        id: String,
        request: TurnRequest,
        semaphore: &Arc<Semaphore>,
        timeout: Duration,
    ) -> Result<TurnSnapshot, ApiError> {
        request.validate()?;
        let fingerprint: [u8; 32] = Sha256::digest(serde_json::to_vec(&request).unwrap()).into();
        let mut data = self.data.lock().unwrap();
        // Test retry identity BEFORE admission: retries don't consume capacity,
        // even after the session has closed.
        if let Some(previous) = data.turns.get(&id) {
            if previous.fingerprint != fingerprint {
                return Err(ApiError::conflict("turn id reused with different payload"));
            }
            return Ok(previous.snapshot.clone());
        }
        if data.status != SessionStatus::Idle || data.active.is_some() {
            return Err(ApiError::conflict("session is not idle"));
        }
        if data.turns.len() >= MAX_TURNS {
            return Err(ApiError::capacity(
                "turn limit reached; create a new session",
            ));
        }
        let permit = semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::capacity("execution capacity exhausted"))?;
        self.commands
            .try_send(Command::Turn(id.clone()))
            .map_err(|_| ApiError::capacity("session command queue unavailable"))?;
        let snapshot = TurnSnapshot {
            turn_id: id.clone(),
            status: TurnStatus::Accepted,
            result: None,
            result_truncated: false,
            error: None,
            usage: None,
        };
        data.turns.insert(
            id.clone(),
            TurnRecord {
                fingerprint,
                request: Some(request),
                snapshot: snapshot.clone(),
            },
        );
        data.active = Some(ActiveTurn {
            id,
            deadline: Instant::now() + timeout,
            completion_outcome: None,
            _permit: permit,
        });
        data.status = SessionStatus::Running;
        drop(data);
        self.notify();
        Ok(snapshot)
    }

    async fn steer(&self, id: String, text: String) -> Result<(), ApiError> {
        if text.trim().is_empty() {
            return Err(ApiError::bad_request("request must not be empty"));
        }
        {
            let data = self.data.lock().unwrap();
            data.require_steerable(&id)?;
        }
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Steer {
                turn_id: id,
                text,
                reply,
            })
            .map_err(|_| ApiError::capacity("session command queue unavailable"))?;
        response
            .await
            .map_err(|_| ApiError::conflict("session stopped before steering"))?
    }

    /// Post (`Some`) or withdraw (`None`, optionally only `withdraw_id`) the
    /// host notice for turn `id`. Returns whether the slot changed.
    async fn notice(
        &self,
        id: String,
        request: Option<NoticeRequest>,
        withdraw_id: Option<String>,
    ) -> Result<bool, ApiError> {
        let change = match request {
            Some(request) => {
                if !valid_notice_id(&request.id) {
                    return Err(ApiError::bad_request(
                        "notice id must be 1-128 characters of A-Z a-z 0-9 - . _ ~",
                    ));
                }
                if request.text.trim().is_empty() {
                    return Err(ApiError::bad_request("notice text must not be empty"));
                }
                if request.expires_at.is_some_and(|at| !at.is_finite()) {
                    return Err(ApiError::bad_request(
                        "expires_at must be finite unix seconds",
                    ));
                }
                self.data.lock().unwrap().require_steerable(&id)?;
                NoticeChange::Post(chaos_kern::host_notice::HostNotice {
                    id: request.id,
                    text: request.text,
                    expires_at: request.expires_at,
                })
            }
            None => NoticeChange::Withdraw(withdraw_id),
        };
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Notice {
                turn_id: id,
                change,
                reply,
            })
            .map_err(|_| ApiError::capacity("session command queue unavailable"))?;
        response
            .await
            .map_err(|_| ApiError::conflict("session stopped before the notice was recorded"))?
    }

    fn interrupt(&self, id: String) -> Result<TurnSnapshot, ApiError> {
        let mut data = self.data.lock().unwrap();
        data.require_active(&id)?;
        if data.turns[&id].snapshot.status != TurnStatus::Interrupting {
            self.commands
                .try_send(Command::Interrupt(id.clone()))
                .map_err(|_| ApiError::capacity("session command queue unavailable"))?;
            data.turns.get_mut(&id).unwrap().snapshot.status = TurnStatus::Interrupting;
        }
        let snapshot = data.turns[&id].snapshot.clone();
        drop(data);
        self.notify();
        Ok(snapshot)
    }

    fn close(&self) -> Result<(), ApiError> {
        let mut data = self.data.lock().unwrap();
        if matches!(
            data.status,
            SessionStatus::Closing | SessionStatus::Closed | SessionStatus::Failed
        ) {
            return Ok(());
        }
        self.commands
            .try_send(Command::Close)
            .map_err(|_| ApiError::capacity("session command queue unavailable"))?;
        data.status = SessionStatus::Closing;
        drop(data);
        self.notify();
        Ok(())
    }

    fn events(&self, after: u64) -> Result<EventBatch, ApiError> {
        let data = self.data.lock().unwrap();
        if after > data.sequence {
            return Err(ApiError::bad_request(
                "event cursor is ahead of this session",
            ));
        }
        let oldest = data.events.front().map(|event| event.sequence);
        Ok(EventBatch {
            events: data
                .events
                .iter()
                .filter(|event| event.sequence > after)
                .cloned()
                .collect(),
            gap: oldest.is_some_and(|oldest| after < oldest - 1),
            next_cursor: data.sequence,
        })
    }
}

impl SessionData {
    fn mark_unknown(&mut self) {
        self.status = SessionStatus::Unknown;
        if let Some(id) = self.active.as_ref().map(|active| active.id.as_str()) {
            self.turns.get_mut(id).unwrap().snapshot.status = TurnStatus::Unknown;
        }
    }

    fn new() -> Self {
        Self {
            status: SessionStatus::Starting,
            process_id: None,
            turns: BTreeMap::new(),
            active: None,
            events: VecDeque::new(),
            replay_bytes: 0,
            sequence: 0,
        }
    }

    fn require_active(&self, id: &str) -> Result<(), ApiError> {
        if self.status != SessionStatus::Running
            || self.active.as_ref().map(|turn| turn.id.as_str()) != Some(id)
        {
            return Err(ApiError::conflict("expected turn is not active"));
        }
        Ok(())
    }

    fn finish_turn(&mut self, id: &str, status: TurnStatus) {
        if self.active.as_ref().map(|turn| turn.id.as_str()) != Some(id) {
            return;
        }
        self.turns.get_mut(id).unwrap().snapshot.status = status;
        self.active = None;
        if self.status == SessionStatus::Running {
            self.status = SessionStatus::Idle;
        }
    }

    fn require_steerable(&self, id: &str) -> Result<(), ApiError> {
        self.require_active(id)?;
        if !matches!(
            self.turns[id].snapshot.status,
            TurnStatus::Accepted | TurnStatus::Running
        ) {
            return Err(ApiError::conflict("foreground turn is no longer steerable"));
        }
        Ok(())
    }

    fn maybe_finish(&mut self, quiescent: bool) {
        if !quiescent {
            return;
        }
        if let Some(active) = &self.active {
            if let Some(outcome) = active.completion_outcome {
                let id = active.id.clone();
                self.finish_turn(&id, outcome);
            }
        } else if self.status == SessionStatus::Running {
            self.status = SessionStatus::Idle;
        }
    }

    fn observe(&mut self, event: &Event, quiescent: bool) {
        if let Some(active) = &self.active {
            let id = active.id.clone();
            let turn = &mut self.turns.get_mut(&id).unwrap().snapshot;
            match &event.msg {
                EventMsg::TurnStarted(started) if started.turn_id == id => {
                    if turn.status == TurnStatus::Accepted {
                        turn.status = TurnStatus::Running;
                    }
                }
                EventMsg::TurnComplete(completed) if completed.turn_id == id => {
                    let text = completed.last_agent_message.as_deref().unwrap_or("");
                    let (text, truncated) = bounded_text(text, MAX_RESULT_BYTES);
                    turn.result = Some(text);
                    turn.result_truncated = truncated;
                    let status = if turn.error.is_some() {
                        TurnStatus::Failed
                    } else {
                        TurnStatus::Completed
                    };
                    self.active.as_mut().unwrap().completion_outcome = Some(status);
                    self.turns.get_mut(&id).unwrap().snapshot.status =
                        TurnStatus::WaitingBackground;
                }
                EventMsg::TurnAborted(aborted) if aborted.turn_id.as_deref() == Some(&id) => {
                    self.active.as_mut().unwrap().completion_outcome =
                        Some(TurnStatus::Interrupted);
                    self.turns.get_mut(&id).unwrap().snapshot.status =
                        TurnStatus::WaitingBackground;
                }
                EventMsg::Error(_) if event.id == id => {
                    // An error alone is not proof of termination.
                    turn.error =
                        Some("agent reported an error; inspect authenticated events".into());
                }
                EventMsg::TokenCount(count) if event.id == id => {
                    if let Some(info) = &count.info {
                        turn.usage = Some(TokenUsageResponse {
                            total_token_usage: usage_entry(&info.total_token_usage),
                            last_token_usage: usage_entry(&info.last_token_usage),
                            model_context_window: info.model_context_window,
                        });
                    }
                }
                _ => {}
            }
        }
        if self.active.is_none() && matches!(event.msg, EventMsg::TurnStarted(_)) {
            self.status = SessionStatus::Running;
        }
        self.maybe_finish(quiescent);
        self.sequence += 1;
        let value = serde_json::to_value(event).expect("kernel Event is JSON serializable");
        let bytes = serde_json::to_vec(&value).unwrap().len();
        let (value, bytes) = if bytes > MAX_EVENT_BYTES {
            let value = serde_json::json!({
                "id": bounded_text(&event.id, 128).0,
                "msg": {"type": "http_event_omitted", "reason": "event_size_limit"}
            });
            let bytes = serde_json::to_vec(&value).unwrap().len();
            (value, bytes)
        } else {
            (value, bytes)
        };
        while self.events.len() >= MAX_EVENTS || self.replay_bytes + bytes > MAX_REPLAY_BYTES {
            self.replay_bytes -= self.events.pop_front().unwrap().bytes;
        }
        self.replay_bytes += bytes;
        self.events.push_back(ReplayEvent {
            sequence: self.sequence,
            event: value,
            bytes,
        });
    }
}

fn bounded_text(text: &str, limit: usize) -> (String, bool) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), end < text.len())
}

fn usage_entry(usage: &chaos_ipc::protocol::TokenUsage) -> TokenUsageEntry {
    TokenUsageEntry {
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        total_tokens: usage.total_tokens,
    }
}

#[cfg(test)]
mod tests;
