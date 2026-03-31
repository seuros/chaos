use crate::AuthManager;
use crate::config::Config;
use crate::default_client::create_client;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone)]
pub(crate) struct TrackEventsContext {
    pub(crate) model_slug: String,
    pub(crate) process_id: String,
    pub(crate) turn_id: String,
}

pub(crate) fn build_track_events_context(
    model_slug: String,
    process_id: String,
    turn_id: String,
) -> TrackEventsContext {
    TrackEventsContext {
        model_slug,
        process_id,
        turn_id,
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InvocationType {
    #[allow(dead_code)]
    Explicit,
    Implicit,
}

pub(crate) struct AppInvocation {
    pub(crate) connector_id: Option<String>,
    pub(crate) app_name: Option<String>,
    pub(crate) invocation_type: Option<InvocationType>,
}

#[derive(Clone)]
pub(crate) struct AnalyticsEventsQueue {
    sender: mpsc::Sender<TrackEventsJob>,
    app_used_emitted_keys: Arc<Mutex<HashSet<(String, String)>>>,
}

#[derive(Clone)]
pub struct AnalyticsEventsClient {
    queue: AnalyticsEventsQueue,
    config: Arc<Config>,
}

impl AnalyticsEventsQueue {
    pub(crate) fn new(auth_manager: Arc<AuthManager>) -> Self {
        let (sender, mut receiver) = mpsc::channel(ANALYTICS_EVENTS_QUEUE_SIZE);
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                match job {
                    TrackEventsJob::AppUsed(job) => {
                        send_track_app_used(&auth_manager, job).await;
                    }
                }
            }
        });
        Self {
            sender,
            app_used_emitted_keys: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn try_send(&self, job: TrackEventsJob) {
        if self.sender.try_send(job).is_err() {
            //TODO: add a metric for this
            tracing::warn!("dropping analytics events: queue is full");
        }
    }

    fn should_enqueue_app_used(&self, tracking: &TrackEventsContext, app: &AppInvocation) -> bool {
        let Some(connector_id) = app.connector_id.as_ref() else {
            return true;
        };
        let mut emitted = self
            .app_used_emitted_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if emitted.len() >= ANALYTICS_EVENT_DEDUPE_MAX_KEYS {
            emitted.clear();
        }
        emitted.insert((tracking.turn_id.clone(), connector_id.clone()))
    }
}

impl AnalyticsEventsClient {
    pub fn new(config: Arc<Config>, auth_manager: Arc<AuthManager>) -> Self {
        Self {
            queue: AnalyticsEventsQueue::new(Arc::clone(&auth_manager)),
            config,
        }
    }

    pub(crate) fn track_app_used(&self, tracking: TrackEventsContext, app: AppInvocation) {
        track_app_used(&self.queue, Arc::clone(&self.config), Some(tracking), app);
    }
}

enum TrackEventsJob {
    AppUsed(TrackAppUsedJob),
}

struct TrackAppUsedJob {
    config: Arc<Config>,
    tracking: TrackEventsContext,
    app: AppInvocation,
}

const ANALYTICS_EVENTS_QUEUE_SIZE: usize = 256;
const ANALYTICS_EVENTS_TIMEOUT: Duration = Duration::from_secs(10);
const ANALYTICS_EVENT_DEDUPE_MAX_KEYS: usize = 4096;

#[derive(Serialize)]
struct TrackEventsRequest {
    events: Vec<TrackEventRequest>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum TrackEventRequest {
    #[allow(dead_code)]
    AppMentioned(CodexAppMentionedEventRequest),
    AppUsed(CodexAppUsedEventRequest),
}

#[derive(Serialize)]
struct CodexAppMetadata {
    connector_id: Option<String>,
    process_id: Option<String>,
    turn_id: Option<String>,
    app_name: Option<String>,
    product_client_id: Option<String>,
    invoke_type: Option<InvocationType>,
    model_slug: Option<String>,
}

#[derive(Serialize)]
struct CodexAppMentionedEventRequest {
    event_type: &'static str,
    event_params: CodexAppMetadata,
}

#[derive(Serialize)]
struct CodexAppUsedEventRequest {
    event_type: &'static str,
    event_params: CodexAppMetadata,
}

pub(crate) fn track_app_used(
    queue: &AnalyticsEventsQueue,
    config: Arc<Config>,
    tracking: Option<TrackEventsContext>,
    app: AppInvocation,
) {
    if config.analytics_enabled == Some(false) {
        return;
    }
    let Some(tracking) = tracking else {
        return;
    };
    if !queue.should_enqueue_app_used(&tracking, &app) {
        return;
    }
    let job = TrackEventsJob::AppUsed(TrackAppUsedJob {
        config,
        tracking,
        app,
    });
    queue.try_send(job);
}

async fn send_track_app_used(auth_manager: &AuthManager, job: TrackAppUsedJob) {
    let TrackAppUsedJob {
        config,
        tracking,
        app,
    } = job;
    let event_params = codex_app_metadata(&tracking, app);
    let events = vec![TrackEventRequest::AppUsed(CodexAppUsedEventRequest {
        event_type: "codex_app_used",
        event_params,
    })];

    send_track_events(auth_manager, config, events).await;
}

fn codex_app_metadata(tracking: &TrackEventsContext, app: AppInvocation) -> CodexAppMetadata {
    CodexAppMetadata {
        connector_id: app.connector_id,
        process_id: Some(tracking.process_id.clone()),
        turn_id: Some(tracking.turn_id.clone()),
        app_name: app.app_name,
        product_client_id: Some(crate::default_client::originator().value),
        invoke_type: app.invocation_type,
        model_slug: Some(tracking.model_slug.clone()),
    }
}

async fn send_track_events(
    auth_manager: &AuthManager,
    config: Arc<Config>,
    events: Vec<TrackEventRequest>,
) {
    if events.is_empty() {
        return;
    }
    let Some(auth) = auth_manager.auth().await else {
        return;
    };
    if !auth.is_chatgpt_auth() {
        return;
    }
    let access_token = match auth.get_token() {
        Ok(token) => token,
        Err(_) => return,
    };
    let Some(account_id) = auth.get_account_id() else {
        return;
    };

    let base_url = config.chatgpt_base_url.trim_end_matches('/');
    let url = format!("{base_url}/codex/analytics-events/events");
    let payload = TrackEventsRequest { events };

    let response = create_client()
        .post(&url)
        .timeout(ANALYTICS_EVENTS_TIMEOUT)
        .bearer_auth(&access_token)
        .header("chatgpt-account-id", &account_id)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await;

    match response {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!("events failed with status {status}: {body}");
        }
        Err(err) => {
            tracing::warn!("failed to send events request: {err}");
        }
    }
}

#[cfg(test)]
#[path = "analytics_client_tests.rs"]
mod tests;
