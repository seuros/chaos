//! Persist session history into journald so sessions can be replayed later.

use std::io::Error as IoError;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chaos_ipc::ProcessId;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_journald::AppendBatchInput as JournalAppendBatchInput;
use chaos_journald::CreateProcessInput as JournalCreateProcessInput;
use chaos_journald::ErrorCode as JournalErrorCode;
use chaos_journald::JournalClientError;
use chaos_journald::JournalEntry;
use chaos_journald::JournalRpcClient;
use jiff::Timestamp;
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::{self};
use tokio::sync::oneshot;
use tracing::info;
use tracing::warn;
use uuid::Uuid;

use super::list::Cursor;
use super::list::ProcessItem;
use super::list::ProcessSortKey;
use super::list::ProcessesPage;
use super::metadata;
use super::policy::EventPersistenceMode;
use super::policy::is_persisted_response_item;
use crate::default_client::originator;
use crate::git_info::collect_git_info;
use crate::path_utils;
use crate::state_db;
use crate::state_db::StateDbHandle;
use crate::truncate::TruncationPolicy;
use crate::truncate::truncate_text;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::ResumedHistory;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionMeta;
use chaos_ipc::protocol::SessionMetaLine;
use chaos_ipc::protocol::SessionSource;
use chaos_journald::LoadedJournal;
use chaos_journald::ProcessRecord as JournalProcessRecord;
use chaos_proc::ProcessMetadataBuilder;
use chaos_proc::StateRuntime;
use chaos_traits::RolloutConfig;

#[derive(Clone)]
pub struct RolloutRecorder {
    tx: Sender<RolloutCmd>,
    state_db: Option<StateDbHandle>,
    event_persistence_mode: EventPersistenceMode,
    live_rollout_items: Arc<Mutex<Vec<RolloutItem>>>,
}

#[derive(Clone)]
pub enum RolloutRecorderParams {
    Create {
        conversation_id: ProcessId,
        forked_from_id: Option<ProcessId>,
        source: SessionSource,
        base_instructions: BaseInstructions,
        dynamic_tools: Vec<DynamicToolSpec>,
        event_persistence_mode: EventPersistenceMode,
    },
    Resume {
        conversation_id: ProcessId,
        source: SessionSource,
        event_persistence_mode: EventPersistenceMode,
    },
}

enum RolloutCmd {
    AddItems(Vec<RolloutItem>),
    Persist {
        ack: oneshot::Sender<()>,
    },
    /// Ensure all prior writes are processed; respond when flushed.
    Flush {
        ack: oneshot::Sender<()>,
    },
    Shutdown {
        ack: oneshot::Sender<()>,
    },
}

impl RolloutRecorderParams {
    pub fn new(
        conversation_id: ProcessId,
        forked_from_id: Option<ProcessId>,
        source: SessionSource,
        base_instructions: BaseInstructions,
        dynamic_tools: Vec<DynamicToolSpec>,
        event_persistence_mode: EventPersistenceMode,
    ) -> Self {
        Self::Create {
            conversation_id,
            forked_from_id,
            source,
            base_instructions,
            dynamic_tools,
            event_persistence_mode,
        }
    }

    pub fn resume(
        conversation_id: ProcessId,
        source: SessionSource,
        event_persistence_mode: EventPersistenceMode,
    ) -> Self {
        Self::Resume {
            conversation_id,
            source,
            event_persistence_mode,
        }
    }
}

const PERSISTED_EXEC_AGGREGATED_OUTPUT_MAX_BYTES: usize = 10_000;

fn sanitize_rollout_item_for_persistence(
    item: RolloutItem,
    mode: EventPersistenceMode,
) -> RolloutItem {
    if mode != EventPersistenceMode::Extended {
        return item;
    }

    match item {
        RolloutItem::EventMsg(EventMsg::ExecCommandEnd(mut event)) => {
            // Persist only a bounded aggregated summary of command output.
            event.aggregated_output = truncate_text(
                &event.aggregated_output,
                TruncationPolicy::Bytes(PERSISTED_EXEC_AGGREGATED_OUTPUT_MAX_BYTES),
            );
            // Drop unnecessary fields from rollout storage since aggregated_output is all we need.
            event.stdout.clear();
            event.stderr.clear();
            event.formatted_output.clear();
            RolloutItem::EventMsg(EventMsg::ExecCommandEnd(event))
        }
        _ => item,
    }
}

impl RolloutRecorder {
    /// List processes persisted in journald.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_processes(
        config: &impl RolloutConfig,
        page_size: usize,
        cursor: Option<&Cursor>,
        sort_key: ProcessSortKey,
        allowed_sources: &[SessionSource],
        model_providers: Option<&[String]>,
        default_provider: &str,
        search_term: Option<&str>,
    ) -> std::io::Result<ProcessesPage> {
        Self::list_processes_from_journal(
            config,
            page_size,
            cursor,
            sort_key,
            allowed_sources,
            model_providers,
            default_provider,
            /*archived*/ false,
            search_term,
        )
        .await
    }

    /// List archived processes persisted in journald.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_archived_processes(
        config: &impl RolloutConfig,
        page_size: usize,
        cursor: Option<&Cursor>,
        sort_key: ProcessSortKey,
        allowed_sources: &[SessionSource],
        model_providers: Option<&[String]>,
        default_provider: &str,
        search_term: Option<&str>,
    ) -> std::io::Result<ProcessesPage> {
        Self::list_processes_from_journal(
            config,
            page_size,
            cursor,
            sort_key,
            allowed_sources,
            model_providers,
            default_provider,
            /*archived*/ true,
            search_term,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn list_processes_from_journal(
        _config: &impl RolloutConfig,
        page_size: usize,
        cursor: Option<&Cursor>,
        sort_key: ProcessSortKey,
        allowed_sources: &[SessionSource],
        model_providers: Option<&[String]>,
        _default_provider: &str,
        archived: bool,
        search_term: Option<&str>,
    ) -> std::io::Result<ProcessesPage> {
        let client = journal_client_from_env_or_bootstrap()
            .await
            .map_err(IoError::other)?;
        let mut records = client
            .list_processes(Some(archived))
            .await
            .map_err(IoError::other)?;
        records.retain(|record| {
            journal_record_matches_filters(record, allowed_sources, model_providers)
        });
        sort_journal_records(&mut records, sort_key);

        let mut items = Vec::with_capacity(page_size);
        let mut scanned = 0usize;
        let mut next_cursor = None;
        let mut last_returned_cursor = None;
        let search_term = search_term.map(str::to_lowercase);
        for record in records {
            scanned = scanned.saturating_add(1);
            if journal_record_is_before_cursor(&record, cursor, sort_key) {
                continue;
            }

            let Some(process_id) = process_uuid(&record.process_id) else {
                continue;
            };
            let loaded = client
                .load_journal(record.process_id)
                .await
                .map_err(IoError::other)?;
            let Some(item) =
                journal_process_item_from_loaded(&record, loaded, search_term.as_deref())
            else {
                continue;
            };

            if items.len() == page_size {
                next_cursor = last_returned_cursor.clone();
                break;
            }
            items.push(item);
            last_returned_cursor = Some(Cursor::new(
                journal_record_sort_timestamp(&record, sort_key),
                process_id,
            ));
        }

        Ok(ProcessesPage {
            items,
            next_cursor,
            num_scanned_records: scanned,
            reached_scan_limit: false,
        })
    }

    /// Find the newest recorded process id, optionally filtering to a matching cwd.
    #[allow(clippy::too_many_arguments)]
    pub async fn find_latest_process_id(
        _config: &impl RolloutConfig,
        page_size: usize,
        cursor: Option<&Cursor>,
        sort_key: ProcessSortKey,
        allowed_sources: &[SessionSource],
        model_providers: Option<&[String]>,
        default_provider: &str,
        filter_cwd: Option<&Path>,
    ) -> std::io::Result<Option<ProcessId>> {
        let client = journal_client_from_env_or_bootstrap()
            .await
            .map_err(IoError::other)?;
        let mut records = client
            .list_processes(Some(false))
            .await
            .map_err(IoError::other)?;
        records.retain(|record| {
            journal_record_matches_filters(record, allowed_sources, model_providers)
        });
        sort_journal_records(&mut records, sort_key);

        let mut matched = 0usize;
        for record in records {
            if journal_record_is_before_cursor(&record, cursor, sort_key) {
                continue;
            }
            if let Some(cwd) = filter_cwd
                && !cwd_matches(record.cwd.as_path(), cwd)
            {
                continue;
            }
            matched = matched.saturating_add(1);
            if matched > page_size {
                break;
            }
            return Ok(Some(record.process_id));
        }
        let _ = default_provider;
        Ok(None)
    }

    /// Attempt to create a new [`RolloutRecorder`].
    ///
    /// Newly created sessions defer persistence until `persist()` is called.
    /// Resumed sessions append new items immediately.
    pub async fn new(
        config: &impl RolloutConfig,
        params: RolloutRecorderParams,
        state_db_ctx: Option<StateDbHandle>,
        state_builder: Option<ProcessMetadataBuilder>,
    ) -> std::io::Result<Self> {
        let (meta, event_persistence_mode, journal_sink, persisted) = match params {
            RolloutRecorderParams::Create {
                conversation_id,
                forked_from_id,
                source,
                base_instructions,
                dynamic_tools,
                event_persistence_mode,
            } => {
                let session_id = conversation_id;
                let started_at = OffsetDateTime::now_utc();
                let journal_source = source.clone();

                let timestamp_format: &[FormatItem] = format_description!(
                    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
                );
                let timestamp = started_at
                    .to_offset(time::UtcOffset::UTC)
                    .format(timestamp_format)
                    .map_err(|e| IoError::other(format!("failed to format timestamp: {e}")))?;

                let session_meta = SessionMeta {
                    id: session_id,
                    forked_from_id,
                    timestamp,
                    cwd: config.cwd().to_path_buf(),
                    originator: originator().value,
                    cli_version: CHAOS_VERSION.to_string(),
                    agent_nickname: source.get_nickname(),
                    agent_role: source.get_agent_role(),
                    source,
                    model_provider: Some(config.model_provider_id().to_string()),
                    base_instructions: Some(base_instructions),
                    dynamic_tools: if dynamic_tools.is_empty() {
                        None
                    } else {
                        Some(dynamic_tools)
                    },
                    memory_mode: (!config.generate_memories()).then_some("disabled".to_string()),
                };

                (
                    Some(session_meta),
                    event_persistence_mode,
                    JournalSink::pending(PendingJournalConfig {
                        process_id: conversation_id,
                        source: journal_source,
                        cwd: config.cwd().to_path_buf(),
                        created_at: Timestamp::now(),
                        model_provider: config.model_provider_id().to_string(),
                        cli_version: CHAOS_VERSION.to_string(),
                        owner_id: Uuid::now_v7().to_string(),
                    }),
                    false,
                )
            }
            RolloutRecorderParams::Resume {
                conversation_id,
                source,
                event_persistence_mode,
            } => (
                None,
                event_persistence_mode,
                JournalSink::pending(PendingJournalConfig {
                    process_id: conversation_id,
                    source,
                    cwd: config.cwd().to_path_buf(),
                    created_at: Timestamp::now(),
                    model_provider: config.model_provider_id().to_string(),
                    cli_version: CHAOS_VERSION.to_string(),
                    owner_id: Uuid::now_v7().to_string(),
                }),
                true,
            ),
        };

        // Clone the cwd for the spawned task to collect git info asynchronously
        let cwd = config.cwd().to_path_buf();

        // A reasonably-sized bounded channel. If the buffer fills up the send
        // future will yield, which is fine – we only need to ensure we do not
        // perform *blocking* I/O on the caller's thread.
        let (tx, rx) = mpsc::channel::<RolloutCmd>(256);
        tokio::task::spawn(rollout_writer(
            persisted,
            rx,
            meta,
            cwd,
            state_db_ctx.clone(),
            state_builder,
            config.model_provider_id().to_string(),
            config.generate_memories(),
            journal_sink,
        ));

        Ok(Self {
            tx,
            state_db: state_db_ctx,
            event_persistence_mode,
            live_rollout_items: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn state_db(&self) -> Option<StateDbHandle> {
        self.state_db.clone()
    }

    pub(crate) async fn record_items(&self, items: &[RolloutItem]) -> std::io::Result<()> {
        let mut filtered = Vec::new();
        for item in items {
            // Note that function calls may look a bit strange if they are
            // "fully qualified MCP tool calls," so we could consider
            // reformatting them in that case.
            if is_persisted_response_item(item, self.event_persistence_mode) {
                filtered.push(sanitize_rollout_item_for_persistence(
                    item.clone(),
                    self.event_persistence_mode,
                ));
            }
        }
        if filtered.is_empty() {
            return Ok(());
        }
        self.live_rollout_items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(filtered.iter().cloned());
        self.tx
            .send(RolloutCmd::AddItems(filtered))
            .await
            .map_err(|e| IoError::other(format!("failed to queue rollout items: {e}")))
    }

    pub fn snapshot_rollout_items(&self) -> Vec<RolloutItem> {
        self.live_rollout_items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Materialize persisted history and commit all buffered items.
    ///
    /// This is idempotent; after first materialization, repeated calls are no-ops.
    pub async fn persist(&self) -> std::io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(RolloutCmd::Persist { ack: tx })
            .await
            .map_err(|e| IoError::other(format!("failed to queue rollout persist: {e}")))?;
        rx.await
            .map_err(|e| IoError::other(format!("failed waiting for rollout persist: {e}")))
    }

    /// Flush all queued writes and wait until they are committed by the writer task.
    pub async fn flush(&self) -> std::io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(RolloutCmd::Flush { ack: tx })
            .await
            .map_err(|e| IoError::other(format!("failed to queue rollout flush: {e}")))?;
        rx.await
            .map_err(|e| IoError::other(format!("failed waiting for rollout flush: {e}")))
    }

    pub async fn get_rollout_history_for_process(
        process_id: ProcessId,
    ) -> std::io::Result<InitialHistory> {
        let client = journal_client_from_env_or_bootstrap()
            .await
            .map_err(IoError::other)?;
        let loaded = match client.load_journal(process_id).await {
            Ok(loaded) => loaded,
            Err(JournalClientError::Remote(payload))
                if payload.code == JournalErrorCode::NotFound =>
            {
                return Err(IoError::other(format!(
                    "journald has no process row for resume target {process_id}; import this session into the journal before resuming it"
                )));
            }
            Err(err) => {
                return Err(IoError::other(format!(
                    "failed to load resume history from journald for {process_id}: {err}"
                )));
            }
        };
        let history: Vec<RolloutItem> = loaded.items.into_iter().map(|entry| entry.item).collect();
        info!(
            process_id = %process_id,
            journal_items = history.len(),
            "Resumed process history directly from journal"
        );
        Ok(InitialHistory::Resumed(ResumedHistory {
            conversation_id: process_id,
            history,
        }))
    }

    pub async fn journal_contains_process(process_id: ProcessId) -> std::io::Result<bool> {
        let client = journal_client_from_env_or_bootstrap()
            .await
            .map_err(IoError::other)?;
        client
            .get_process(process_id)
            .await
            .map(|process| process.is_some())
            .map_err(IoError::other)
    }

    pub async fn read_process_cwd_from_journal(
        process_id: ProcessId,
    ) -> std::io::Result<Option<PathBuf>> {
        let client = journal_client_from_env_or_bootstrap()
            .await
            .map_err(IoError::other)?;
        let loaded = match client.load_journal(process_id).await {
            Ok(loaded) => loaded,
            Err(JournalClientError::Remote(payload))
                if payload.code == JournalErrorCode::NotFound =>
            {
                return Ok(None);
            }
            Err(err) => {
                return Err(IoError::other(format!(
                    "failed to load journal history for cwd lookup on {process_id}: {err}"
                )));
            }
        };

        for entry in loaded.items.iter().rev() {
            if let RolloutItem::TurnContext(item) = &entry.item {
                return Ok(Some(item.cwd.clone()));
            }
        }
        for entry in loaded.items {
            if let RolloutItem::SessionMeta(item) = entry.item {
                return Ok(Some(item.meta.cwd));
            }
        }
        Ok(None)
    }

    pub async fn shutdown(&self) -> std::io::Result<()> {
        let (tx_done, rx_done) = oneshot::channel();
        match self.tx.send(RolloutCmd::Shutdown { ack: tx_done }).await {
            Ok(_) => rx_done
                .await
                .map_err(|e| IoError::other(format!("failed waiting for rollout shutdown: {e}")))?,
            Err(e) => {
                warn!("failed to send rollout shutdown command: {e}");
                return Err(IoError::other(format!(
                    "failed to send rollout shutdown command: {e}"
                )));
            }
        };
        Ok(())
    }
}

const JOURNALD_SOCKET_ENV: &str = "CHAOS_JOURNALD_SOCKET";
const JOURNALD_BIN_ENV: &str = "CHAOS_JOURNALD_BIN";
const JOURNAL_LEASE_TTL: Duration = Duration::from_secs(30);
const JOURNAL_LEASE_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct PendingJournalConfig {
    process_id: ProcessId,
    source: SessionSource,
    cwd: PathBuf,
    created_at: Timestamp,
    model_provider: String,
    cli_version: String,
    owner_id: String,
}

enum JournalSink {
    Disabled,
    Pending(PendingJournalConfig),
    Active(ActiveJournalWriter),
}

struct ActiveJournalWriter {
    client: JournalRpcClient,
    process_id: ProcessId,
    owner_id: String,
    lease_token: String,
    next_seq: i64,
    last_lease_refresh: Instant,
}

impl JournalSink {
    fn pending(config: PendingJournalConfig) -> Self {
        Self::Pending(config)
    }

    async fn append_items(&mut self, items: &[RolloutItem]) {
        if items.is_empty() {
            return;
        }

        let pending = match std::mem::replace(self, Self::Disabled) {
            Self::Disabled => {
                *self = Self::Disabled;
                return;
            }
            Self::Pending(config) => match ActiveJournalWriter::connect(config).await {
                Ok(writer) => writer,
                Err(err) => {
                    warn!("failed to initialize journald dual-write sink: {err}");
                    *self = Self::Disabled;
                    return;
                }
            },
            Self::Active(writer) => writer,
        };

        let mut writer = pending;
        if let Err(err) = writer.append_items(items).await {
            warn!("journald dual-write disabled after append failure: {err}");
            *self = Self::Disabled;
            return;
        }

        *self = Self::Active(writer);
    }

    async fn shutdown(&mut self) {
        let state = std::mem::replace(self, Self::Disabled);
        if let Self::Active(writer) = state
            && let Err(err) = writer.release_lease().await
        {
            warn!("failed to release journald lease: {err}");
        }
    }
}

impl ActiveJournalWriter {
    async fn connect(config: PendingJournalConfig) -> Result<Self, String> {
        let client = journal_client_from_env_or_bootstrap().await?;

        let create_input = JournalCreateProcessInput {
            process_id: config.process_id,
            parent: None,
            source: config.source.clone(),
            cwd: config.cwd.clone(),
            created_at: config.created_at,
            title: None,
            model_provider: Some(config.model_provider.clone()),
            cli_version: Some(config.cli_version.clone()),
        };

        match client.create_process(create_input).await {
            Ok(_) => {}
            Err(JournalClientError::Remote(payload))
                if payload.code == JournalErrorCode::AlreadyExists => {}
            Err(err) => {
                return Err(format!("create_process failed: {err}"));
            }
        }

        let lease = client
            .acquire_lease(
                config.process_id,
                config.owner_id.clone(),
                JOURNAL_LEASE_TTL.as_millis() as u64,
            )
            .await
            .map_err(|err| format!("acquire_lease failed: {err}"))?;
        let loaded = client
            .load_journal(config.process_id)
            .await
            .map_err(|err| format!("load_journal failed: {err}"))?;

        Ok(Self {
            client,
            process_id: config.process_id,
            owner_id: config.owner_id,
            lease_token: lease.lease_token,
            next_seq: loaded.next_seq,
            last_lease_refresh: Instant::now(),
        })
    }

    async fn append_items(&mut self, items: &[RolloutItem]) -> Result<(), String> {
        self.ensure_lease().await?;

        let expected_next_seq = self.next_seq;
        let journal_items = items
            .iter()
            .cloned()
            .enumerate()
            .map(|(offset, item)| JournalEntry {
                seq: expected_next_seq + offset as i64,
                recorded_at: Timestamp::now(),
                item,
            })
            .collect();

        let result = self
            .client
            .append_batch(JournalAppendBatchInput {
                process_id: self.process_id,
                owner_id: self.owner_id.clone(),
                lease_token: self.lease_token.clone(),
                expected_next_seq,
                items: journal_items,
            })
            .await
            .map_err(|err| format!("append_batch failed: {err}"))?;

        self.next_seq = result.next_seq;
        Ok(())
    }

    async fn ensure_lease(&mut self) -> Result<(), String> {
        if self.last_lease_refresh.elapsed() < JOURNAL_LEASE_REFRESH_INTERVAL {
            return Ok(());
        }

        match self
            .client
            .heartbeat_lease(
                self.process_id,
                self.owner_id.clone(),
                self.lease_token.clone(),
                JOURNAL_LEASE_TTL.as_millis() as u64,
            )
            .await
        {
            Ok(lease) => {
                self.lease_token = lease.lease_token;
                self.last_lease_refresh = Instant::now();
                Ok(())
            }
            Err(JournalClientError::Remote(payload))
                if matches!(
                    payload.code,
                    JournalErrorCode::LeaseExpired | JournalErrorCode::InvalidLease
                ) =>
            {
                let lease = self
                    .client
                    .acquire_lease(
                        self.process_id,
                        self.owner_id.clone(),
                        JOURNAL_LEASE_TTL.as_millis() as u64,
                    )
                    .await
                    .map_err(|err| format!("reacquire_lease failed: {err}"))?;
                let loaded = self
                    .client
                    .load_journal(self.process_id)
                    .await
                    .map_err(|err| format!("reload_journal after lease refresh failed: {err}"))?;
                self.lease_token = lease.lease_token;
                self.next_seq = loaded.next_seq;
                self.last_lease_refresh = Instant::now();
                Ok(())
            }
            Err(err) => Err(format!("heartbeat_lease failed: {err}")),
        }
    }

    async fn release_lease(self) -> Result<(), String> {
        self.client
            .release_lease(self.process_id, self.owner_id, self.lease_token)
            .await
            .map_err(|err| format!("release_lease failed: {err}"))
    }
}

async fn journal_client_from_env_or_bootstrap() -> Result<JournalRpcClient, String> {
    if let Some(socket_path) = std::env::var_os(JOURNALD_SOCKET_ENV)
        && !socket_path.is_empty()
    {
        return Ok(JournalRpcClient::new(PathBuf::from(socket_path)));
    }

    let binary_path = std::env::var_os(JOURNALD_BIN_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let (client, _paths) = JournalRpcClient::default_or_bootstrap(binary_path.as_deref())
        .await
        .map_err(|err| err.to_string())?;
    Ok(client)
}

fn journal_record_matches_filters(
    record: &JournalProcessRecord,
    allowed_sources: &[SessionSource],
    model_providers: Option<&[String]>,
) -> bool {
    if !allowed_sources.is_empty() && !allowed_sources.contains(&record.source) {
        return false;
    }
    if let Some(model_providers) = model_providers
        && !model_providers
            .iter()
            .any(|provider| provider == &record.model_provider)
    {
        return false;
    }
    true
}

fn sort_journal_records(records: &mut [JournalProcessRecord], sort_key: ProcessSortKey) {
    records.sort_by(|left, right| {
        journal_record_sort_timestamp(right, sort_key)
            .cmp(&journal_record_sort_timestamp(left, sort_key))
            .then_with(|| {
                process_uuid(&right.process_id)
                    .unwrap_or(Uuid::nil())
                    .cmp(&process_uuid(&left.process_id).unwrap_or(Uuid::nil()))
            })
    });
}

fn journal_record_sort_timestamp(
    record: &JournalProcessRecord,
    sort_key: ProcessSortKey,
) -> OffsetDateTime {
    let seconds = match sort_key {
        ProcessSortKey::CreatedAt => record.created_at.as_second(),
        ProcessSortKey::UpdatedAt => record.updated_at.as_second(),
    };
    OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn journal_record_is_before_cursor(
    record: &JournalProcessRecord,
    cursor: Option<&Cursor>,
    sort_key: ProcessSortKey,
) -> bool {
    let Some(cursor) = cursor else {
        return false;
    };
    let ts = journal_record_sort_timestamp(record, sort_key);
    let id = process_uuid(&record.process_id).unwrap_or(Uuid::nil());
    ts > cursor.ts() || (ts == cursor.ts() && id >= cursor.id())
}

fn process_uuid(process_id: &ProcessId) -> Option<Uuid> {
    Uuid::parse_str(&process_id.to_string()).ok()
}

fn journal_process_item_from_loaded(
    record: &JournalProcessRecord,
    loaded: LoadedJournal,
    search_term: Option<&str>,
) -> Option<ProcessItem> {
    let mut first_user_message = None;
    let mut saw_user_event = false;
    let mut git_branch = None;
    let mut git_sha = None;
    let mut git_origin_url = None;

    for entry in loaded.items {
        match entry.item {
            RolloutItem::SessionMeta(session_meta_line) => {
                if let Some(git) = session_meta_line.git {
                    if git_branch.is_none() {
                        git_branch = git.branch;
                    }
                    if git_sha.is_none() {
                        git_sha = git.commit_hash;
                    }
                    if git_origin_url.is_none() {
                        git_origin_url = git.repository_url;
                    }
                }
            }
            RolloutItem::EventMsg(EventMsg::UserMessage(user_message)) => {
                saw_user_event = true;
                if first_user_message.is_none() {
                    first_user_message = Some(user_message.message);
                }
            }
            RolloutItem::ResponseItem(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::EventMsg(_) => {}
        }
    }

    if !saw_user_event {
        return None;
    }

    if let Some(term) = search_term {
        let term = term.trim();
        if !term.is_empty() {
            let preview_match = first_user_message
                .as_ref()
                .is_some_and(|message| message.to_lowercase().contains(term));
            let title_match = record.title.to_lowercase().contains(term);
            let cwd_match = record.cwd.to_string_lossy().to_lowercase().contains(term);
            let branch_match = git_branch
                .as_ref()
                .is_some_and(|branch| branch.to_lowercase().contains(term));
            if !(preview_match || title_match || cwd_match || branch_match) {
                return None;
            }
        }
    }

    Some(ProcessItem {
        process_id: Some(record.process_id),
        first_user_message,
        cwd: Some(record.cwd.clone()),
        git_branch,
        git_sha,
        git_origin_url,
        source: Some(record.source.clone()),
        agent_nickname: record.agent_nickname.clone(),
        agent_role: record.agent_role.clone(),
        model_provider: Some(record.model_provider.clone()),
        cli_version: record.cli_version.clone(),
        created_at: Some(record.created_at.to_string()),
        updated_at: Some(record.updated_at.to_string()),
    })
}

#[allow(clippy::too_many_arguments)]
async fn rollout_writer(
    mut persisted: bool,
    mut rx: mpsc::Receiver<RolloutCmd>,
    mut meta: Option<SessionMeta>,
    cwd: std::path::PathBuf,
    state_db_ctx: Option<StateDbHandle>,
    mut state_builder: Option<ProcessMetadataBuilder>,
    default_provider: String,
    generate_memories: bool,
    mut journal_sink: JournalSink,
) -> std::io::Result<()> {
    let mut buffered_items = Vec::<RolloutItem>::new();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            RolloutCmd::AddItems(items) => {
                if items.is_empty() {
                    continue;
                }

                if !persisted {
                    buffered_items.extend(items);
                    continue;
                }

                write_and_reconcile_items(
                    items.as_slice(),
                    state_db_ctx.as_deref(),
                    state_builder.as_ref(),
                    default_provider.as_str(),
                    &mut journal_sink,
                )
                .await?;
            }
            RolloutCmd::Persist { ack } => {
                if !persisted {
                    if let Some(session_meta) = meta.take() {
                        write_session_meta(
                            session_meta,
                            &cwd,
                            state_db_ctx.as_deref(),
                            &mut state_builder,
                            default_provider.as_str(),
                            generate_memories,
                            &mut journal_sink,
                        )
                        .await?;
                    }
                    if !buffered_items.is_empty() {
                        write_and_reconcile_items(
                            buffered_items.as_slice(),
                            state_db_ctx.as_deref(),
                            state_builder.as_ref(),
                            default_provider.as_str(),
                            &mut journal_sink,
                        )
                        .await?;
                        buffered_items.clear();
                    }
                    persisted = true;
                }
                let _ = ack.send(());
            }
            RolloutCmd::Flush { ack } => {
                let _ = ack.send(());
            }
            RolloutCmd::Shutdown { ack } => {
                journal_sink.shutdown().await;
                let _ = ack.send(());
            }
        }
    }

    journal_sink.shutdown().await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_session_meta(
    session_meta: SessionMeta,
    cwd: &Path,
    state_db_ctx: Option<&StateRuntime>,
    state_builder: &mut Option<ProcessMetadataBuilder>,
    default_provider: &str,
    generate_memories: bool,
    journal_sink: &mut JournalSink,
) -> std::io::Result<()> {
    let git_info = collect_git_info(cwd).await;
    let session_meta_line = SessionMetaLine {
        meta: session_meta,
        git: git_info,
    };
    if state_db_ctx.is_some() {
        *state_builder = metadata::builder_from_session_meta(&session_meta_line);
    }

    let rollout_item = RolloutItem::SessionMeta(session_meta_line);
    journal_sink
        .append_items(std::slice::from_ref(&rollout_item))
        .await;
    sync_process_state_after_write(
        state_db_ctx,
        state_builder.as_ref(),
        std::slice::from_ref(&rollout_item),
        default_provider,
        (!generate_memories).then_some("disabled"),
    )
    .await;
    Ok(())
}

async fn write_and_reconcile_items(
    items: &[RolloutItem],
    state_db_ctx: Option<&StateRuntime>,
    state_builder: Option<&ProcessMetadataBuilder>,
    default_provider: &str,
    journal_sink: &mut JournalSink,
) -> std::io::Result<()> {
    journal_sink.append_items(items).await;
    sync_process_state_after_write(
        state_db_ctx,
        state_builder,
        items,
        default_provider,
        /*new_process_memory_mode*/ None,
    )
    .await;
    Ok(())
}

async fn sync_process_state_after_write(
    state_db_ctx: Option<&StateRuntime>,
    state_builder: Option<&ProcessMetadataBuilder>,
    items: &[RolloutItem],
    default_provider: &str,
    new_process_memory_mode: Option<&str>,
) {
    let updated_at = Timestamp::now();
    if new_process_memory_mode.is_some()
        || items
            .iter()
            .any(chaos_proc::rollout_item_affects_process_metadata)
    {
        state_db::apply_rollout_items(
            state_db_ctx,
            default_provider,
            state_builder,
            items,
            "rollout_writer",
            new_process_memory_mode,
            Some(updated_at),
        )
        .await;
        return;
    }

    let process_id = state_builder
        .map(|builder| builder.id)
        .or_else(|| metadata::builder_from_items(items).map(|builder| builder.id));
    if state_db::touch_process_updated_at(state_db_ctx, process_id, updated_at, "rollout_writer")
        .await
    {
        return;
    }
    state_db::apply_rollout_items(
        state_db_ctx,
        default_provider,
        state_builder,
        items,
        "rollout_writer",
        new_process_memory_mode,
        Some(updated_at),
    )
    .await;
}

fn cwd_matches(session_cwd: &Path, cwd: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (
        path_utils::normalize_for_path_comparison(session_cwd),
        path_utils::normalize_for_path_comparison(cwd),
    ) {
        return ca == cb;
    }
    session_cwd == cwd
}
